//! Sending requests: headers, the "no progress" timeout, retries and error answers.

use std::future::Future;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::{Either, select};
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Method, Response, StatusCode, Url};
use serde_json::{Map, Value};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::time::Instant;

use crate::client::{CLIENT_HEADER, CLIENT_NAME, Client, VERSION};
use crate::error::{ApiError, ConnectionError, Error, Result};

/// The most a JSON answer may hold.
const MAX_JSON_BYTES: usize = 16 << 20;
/// The most of an error answer that is read.
const MAX_ERROR_BYTES: usize = 64 << 10;
/// Uploads are sent in pieces of this size (each one counts as progress).
const CHUNK_BYTES: usize = 64 << 10;

/// One API request, sent again by the retry loops when that is safe.
pub(crate) struct Call {
    pub(crate) method: Method,
    pub(crate) url: Url,
    pub(crate) body: CallBody,
    pub(crate) headers: Vec<(&'static str, String)>,
    /// Send the API key (only ever to the API's own address).
    pub(crate) auth: bool,
    /// How long the API may hold the answer (long polling).
    pub(crate) hold: Duration,
}

impl Call {
    pub(crate) fn new(method: Method, url: Url) -> Call {
        Call {
            method,
            url,
            body: CallBody::None,
            headers: Vec::new(),
            auth: false,
            hold: Duration::ZERO,
        }
    }

    pub(crate) fn auth(mut self) -> Call {
        self.auth = true;
        self
    }

    pub(crate) fn json(mut self, body: &Value) -> Call {
        self.body = CallBody::Json(serde_json::to_vec(body).unwrap_or_default());
        self
    }

    pub(crate) fn header(mut self, name: &'static str, value: impl Into<String>) -> Call {
        self.headers.push((name, value.into()));
        self
    }
}

pub(crate) enum CallBody {
    None,
    Json(Vec<u8>),
    /// A part of a file to upload, read again for every attempt.
    Part {
        source: Arc<Source>,
        offset: u64,
        length: u64,
    },
}

/// A file to upload, readable at any offset so that a part can be sent again.
pub(crate) enum Source {
    File(PathBuf),
    Bytes(Bytes),
}

/// The time of the last piece of a request sent or received, for the "no progress" timeout.
#[derive(Clone)]
pub(crate) struct Progress {
    start: Instant,
    /// Milliseconds after `start` of the last piece, plus 1; 0: none yet.
    last: Arc<AtomicU64>,
}

impl Progress {
    fn new() -> Progress {
        Progress {
            start: Instant::now(),
            last: Arc::new(AtomicU64::new(0)),
        }
    }

    fn kick(&self) {
        let millis = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX - 1);
        self.last.store(millis + 1, Ordering::Relaxed);
    }

    /// When the request times out: `timeout` after the last piece, or `timeout + hold` after
    /// the start when nothing has moved yet.
    fn deadline(&self, timeout: Duration, hold: Duration) -> Instant {
        match self.last.load(Ordering::Relaxed) {
            0 => self.start + timeout + hold,
            last => self.start + Duration::from_millis(last - 1) + timeout,
        }
    }
}

/// Runs `fut` until it ends or makes no progress for the timeout.
async fn watch<F: Future>(
    fut: F,
    progress: &Progress,
    timeout: Duration,
    hold: Duration,
) -> Option<F::Output> {
    let mut fut = pin!(fut);
    loop {
        let deadline = progress.deadline(timeout, hold);
        let sleep = pin!(tokio::time::sleep_until(deadline));
        match select(fut.as_mut(), sleep).await {
            Either::Left((out, _)) => return Some(out),
            Either::Right(((), _)) => {
                if progress.deadline(timeout, hold) <= Instant::now() {
                    return None;
                }
            }
        }
    }
}

impl Client {
    /// Sends `call` and returns the body and headers of a successful answer, again after
    /// network errors (also while the body is read) and 408, 429 and 5xx answers.
    pub(crate) async fn call_json(&self, call: &Call) -> Result<(Vec<u8>, HeaderMap)> {
        let mut attempt = 0;
        loop {
            let result = async {
                let (response, progress) = self.send(call).await?;
                let headers = response.headers().clone();
                let body = self
                    .read_body(response, &progress, MAX_JSON_BYTES, &call.url)
                    .await?;
                Ok((body, headers))
            }
            .await;
            match result {
                Ok(answer) => return Ok(answer),
                Err(e) => self.before_retry(e, &mut attempt).await?,
            }
        }
    }

    /// Sends `call` and decodes the answer's JSON.
    pub(crate) async fn call_value<T: serde::de::DeserializeOwned>(
        &self,
        call: &Call,
    ) -> Result<T> {
        let (body, _) = self.call_json(call).await?;
        serde_json::from_slice(&body).map_err(|e| Error::InvalidResponse(e.to_string()))
    }

    /// Returns `Ok` after the pause before the next attempt, or the error when it is final.
    pub(crate) async fn before_retry(&self, error: Error, attempt: &mut u32) -> Result<()> {
        let wait = match &error {
            _ if *attempt >= self.inner.max_retries => return Err(error),
            Error::Connection(e) if e.retryable => None,
            Error::Api(e) if retry_status(e.status) => e.retry_after,
            _ => return Err(error),
        };
        let pause = retry_delay(*attempt, wait);
        *attempt += 1;
        self.pause(pause).await;
        Ok(())
    }

    /// Makes one attempt: a 2xx answer (its body unread), or the error. The attempt times out
    /// when it goes without progress for the client's timeout (plus `hold` while the API holds
    /// the answer).
    pub(crate) async fn send(&self, call: &Call) -> Result<(Response, Progress)> {
        let inner = &self.inner;
        let mut request = inner
            .http
            .request(call.method.clone(), call.url.clone())
            .header(CLIENT_HEADER, format!("{CLIENT_NAME}/{VERSION}"))
            .header(reqwest::header::USER_AGENT, &inner.user_agent);
        if call.auth {
            let key = inner.api_key.as_deref().ok_or(Error::MissingApiKey)?;
            let mut value = HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| Error::Config("The API key must be visible ASCII text.".to_owned()))?;
            value.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }
        if !matches!(call.body, CallBody::Part { .. }) {
            request = request.header(reqwest::header::ACCEPT, "application/json");
        }
        for (name, value) in &call.headers {
            request = request.header(*name, value);
        }
        let progress = Progress::new();
        match &call.body {
            CallBody::None => {}
            CallBody::Json(data) => {
                request = request
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(data.clone());
            }
            CallBody::Part {
                source,
                offset,
                length,
            } => {
                let body = part_body(source, *offset, *length, progress.clone()).await?;
                request = request
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .header(reqwest::header::CONTENT_LENGTH, length.to_string())
                    .body(body);
            }
        }
        let response = match watch(request.send(), &progress, inner.timeout, call.hold).await {
            None => return Err(timed_out(&call.url)),
            Some(Err(e)) => return Err(network_error(&call.url, e)),
            Some(Ok(response)) => response,
        };
        progress.kick();
        if !response.status().is_success() {
            return Err(self.error_from(response, &progress, &call.url).await);
        }
        Ok((response, progress))
    }

    /// Reads a whole answer (at most `limit` bytes), each piece within the client's timeout.
    pub(crate) async fn read_body(
        &self,
        mut response: Response,
        progress: &Progress,
        limit: usize,
        url: &Url,
    ) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        while let Some(chunk) = self.next_chunk(&mut response, progress, url).await? {
            if body.len() + chunk.len() > limit {
                return Err(Error::InvalidResponse(
                    "the answer is larger than expected".to_owned(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// The next piece of an answer, within the client's timeout.
    pub(crate) async fn next_chunk(
        &self,
        response: &mut Response,
        progress: &Progress,
        url: &Url,
    ) -> Result<Option<Bytes>> {
        match watch(
            response.chunk(),
            progress,
            self.inner.timeout,
            Duration::ZERO,
        )
        .await
        {
            None => Err(timed_out(url)),
            Some(Err(e)) => Err(network_error(url, e)),
            Some(Ok(chunk)) => {
                progress.kick();
                Ok(chunk)
            }
        }
    }

    /// Reads (at most 64 KB of) an error answer. The API sends
    /// `{"error": {"code", "message", "params"}}`; the download route sends `{"error": "code"}`.
    async fn error_from(&self, mut response: Response, progress: &Progress, url: &Url) -> Error {
        let status = response.status();
        let retry_after = retry_after(response.headers());
        let mut data = Vec::new();
        while data.len() < MAX_ERROR_BYTES {
            match self.next_chunk(&mut response, progress, url).await {
                Ok(Some(chunk)) => data.extend_from_slice(&chunk),
                _ => break,
            }
        }
        Error::Api(Box::new(api_error(status, &data, retry_after)))
    }
}

/// The body of an upload part: the bytes from `offset` on, in pieces that count as progress.
async fn part_body(
    source: &Source,
    offset: u64,
    length: u64,
    progress: Progress,
) -> Result<reqwest::Body> {
    match source {
        Source::Bytes(bytes) => {
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            let end = start
                .saturating_add(usize::try_from(length).unwrap_or(usize::MAX))
                .min(bytes.len());
            let part = bytes.slice(start..end);
            let pieces = (0..part.len())
                .step_by(CHUNK_BYTES)
                .map(move |at| part.slice(at..(at + CHUNK_BYTES).min(part.len())))
                .collect::<Vec<_>>();
            let stream = futures_util::stream::iter(pieces.into_iter().map(move |piece| {
                progress.kick();
                Ok::<Bytes, std::io::Error>(piece)
            }));
            Ok(reqwest::Body::wrap_stream(stream))
        }
        Source::File(path) => {
            let mut file = tokio::fs::File::open(path).await?;
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            let stream = futures_util::stream::unfold(
                (file, length, progress),
                |(mut file, remaining, progress)| async move {
                    if remaining == 0 {
                        return None;
                    }
                    let size = usize::try_from(remaining)
                        .unwrap_or(CHUNK_BYTES)
                        .min(CHUNK_BYTES);
                    let mut buf = vec![0; size];
                    match file.read(&mut buf).await {
                        Ok(0) => Some((
                            Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "the file got shorter while it was being sent",
                            )),
                            (file, 0, progress),
                        )),
                        Ok(n) => {
                            buf.truncate(n);
                            progress.kick();
                            Some((Ok(Bytes::from(buf)), (file, remaining - n as u64, progress)))
                        }
                        Err(e) => Some((Err(e), (file, 0, progress))),
                    }
                },
            );
            Ok(reqwest::Body::wrap_stream(stream))
        }
    }
}

/// The host (and port) of a URL: errors name only that, since links carry tokens.
fn host_of(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        _ => "the server".to_owned(),
    }
}

fn timed_out(url: &Url) -> Error {
    Error::Connection(Box::new(ConnectionError {
        host: host_of(url),
        timeout: true,
        source: None,
        retryable: true,
    }))
}

/// A network error of reqwest, without the URL (links carry tokens in their query).
fn network_error(url: &Url, error: reqwest::Error) -> Error {
    if error.is_builder() {
        return Error::InvalidArgument(format!(
            "The request cannot be made: {}",
            error.without_url()
        ));
    }
    // A certificate that does not verify is not sent again (rustls reports it as invalid data).
    let mut retryable = true;
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    while let Some(e) = cause {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if io.kind() == std::io::ErrorKind::InvalidData {
                retryable = false;
            }
        }
        cause = e.source();
    }
    Error::Connection(Box::new(ConnectionError {
        host: host_of(url),
        timeout: error.is_timeout(),
        source: Some(Box::new(error.without_url())),
        retryable,
    }))
}

fn retry_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

/// The pause before retry `attempt + 1`: the API's `Retry-After` (at most a minute), or half a
/// second doubling up to 8 seconds, with jitter.
pub(crate) fn retry_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(wait) = retry_after {
        return wait.min(Duration::from_secs(60));
    }
    let base = if attempt < 4 {
        Duration::from_millis(500) * (1 << attempt)
    } else {
        Duration::from_secs(8)
    };
    let half = base / 2;
    let jitter = u32::try_from(random_u64() % 1000).unwrap_or(0);
    half + half * jitter / 1000
}

/// The `Retry-After` header in seconds (an HTTP date counts as unknown).
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let seconds: f64 = value.trim().parse().ok()?;
    (seconds.is_finite() && seconds > 0.0).then(|| Duration::from_secs_f64(seconds.min(86_400.0)))
}

/// The site's English messages for the codes the download route sends without one.
fn fallback_message(code: &str) -> Option<&'static str> {
    Some(match code {
        "fileExpired" => "This file has already been deleted. Please convert it again.",
        "notFound" => "We couldn't find that conversion.",
        "tooManyRequests" => "You're going a bit fast. Please wait a moment and try again.",
        "serverBusy" => "Our servers are busy right now. Please try again in a few minutes.",
        "invalidRequest" => "Something about that request wasn't right.",
        "invalidApiKey" => {
            "This API key does not work. Create one on your account page at converter.studio."
        }
        _ => return None,
    })
}

pub(crate) fn api_error(
    status: StatusCode,
    data: &[u8],
    retry_after: Option<Duration>,
) -> ApiError {
    let mut code = None;
    let mut message = None;
    let mut params = Map::new();
    if let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(data) {
        match body.get("error") {
            Some(Value::Object(info)) => {
                code = info.get("code").and_then(Value::as_str).map(str::to_owned);
                message = info
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(Value::Object(p)) = info.get("params") {
                    params.clone_from(p);
                }
            }
            Some(Value::String(c)) => {
                code = Some(c.clone());
                if let Some(Value::Object(p)) = body.get("params") {
                    params.clone_from(p);
                }
            }
            _ => {}
        }
    }
    let message = message
        .filter(|m| !m.is_empty())
        .or_else(|| {
            code.as_deref()
                .and_then(fallback_message)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            let reason = status.canonical_reason().unwrap_or("");
            format!(
                "The Converter Studio API answered {} {reason}",
                status.as_u16()
            )
            .trim_end()
            .to_owned()
                + "."
        });
    ApiError {
        status: status.as_u16(),
        code,
        message,
        params,
        retry_after,
    }
}

/// 64 random bits from the standard library's hasher keys (seeded by the operating system),
/// a counter and the time: for idempotency keys and jitter, not for secrets.
pub(crate) fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(now.as_nanos());
    }
    hasher.finish()
}
