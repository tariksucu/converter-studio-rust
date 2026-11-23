//! Converting: starting conversions, waiting for them and confirming their price.

use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use reqwest::Method;
use serde_json::{Map, Value, json};
use tokio::time::Instant;

use crate::client::Client;
use crate::error::{
    ConfirmationRequiredError, ConversionFailedError, Error, Result, WaitTimeoutError,
};
use crate::models::{Conversion, Status};
use crate::request::{Call, random_u64};

/// The API holds a status request up to this many seconds until the conversion settles.
const LONG_POLL_SECONDS: u64 = 25;

/// The settings both [`ConvertParams`] and [`ConversionRequest`] carry.
#[derive(Clone, Debug, Default)]
struct Settings {
    options: Map<String, Value>,
    max_credits: Option<u32>,
    callback_url: Option<String>,
    idempotency_key: Option<String>,
}

macro_rules! settings_methods {
    () => {
        /// Adds one of the API's conversion settings, the same as on the website, such as
        /// `option("quality", "high")` for video and JPG, `option("height", "720")` for video,
        /// `option("keepMetadata", true)` for photos, `option("pageRange", "1-3, 5")` for
        /// documents to PDF or `option("ocr", true)` for scanned PDFs to Word. Settings that
        /// do not apply to a conversion are ignored; a name the API does not know is refused
        /// (400). The full list is on <https://converter.studio/developers>.
        pub fn option(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
            self.settings.options.insert(name.into(), value.into());
            self
        }

        /// The price up to which the conversion goes ahead on its own (the API's default is
        /// 20 credits). A dearer one waits for [`Client::confirm`] and costs nothing until
        /// then. 0 only gets the price (see [`quote_only`](Self::quote_only)).
        pub fn max_credits(mut self, credits: u32) -> Self {
            self.settings.max_credits = Some(credits);
            self
        }

        /// Only gets the price: the conversion waits for [`Client::confirm`] whatever it costs.
        pub fn quote_only(self) -> Self {
            self.max_credits(0)
        }

        /// An `https://` address on the public internet that Converter Studio POSTs to when
        /// the conversion waits for a go-ahead (`conversion.quoted`) and when it ends
        /// (`conversion.done`, `conversion.failed` or `conversion.canceled`). Callbacks are
        /// not signed: keep the address secret, and check the conversion with
        /// [`Client::conversion`] before acting on one.
        pub fn callback_url(mut self, url: impl Into<String>) -> Self {
            self.settings.callback_url = Some(url.into());
            self
        }

        /// Makes a repeated request (within 24 hours) return the first conversion instead of
        /// starting and charging a second one; the same key with a different request fails
        /// with a 409 (`idempotencyKeyReused`). Without it, the client makes one up for each
        /// call and keeps it for its own retries; pass your own when your code itself may
        /// repeat a request (a job queue retrying a task). 1 to 255 visible ASCII characters.
        pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
            self.settings.idempotency_key = Some(key.into());
            self
        }
    };
}

/// Settings of [`Client::convert_file`], [`Client::convert_bytes`] and
/// [`Client::convert_url`]. The default converts with the default settings and waits.
///
/// ```
/// use converter_studio::ConvertParams;
///
/// let params = ConvertParams::new()
///     .option("quality", "high")
///     .option("height", "720")
///     .max_credits(5);
/// ```
#[derive(Clone, Debug, Default)]
#[must_use]
pub struct ConvertParams {
    settings: Settings,
    no_wait: bool,
}

impl ConvertParams {
    /// The default settings: the API's defaults, waiting until the conversion is done.
    pub fn new() -> Self {
        ConvertParams::default()
    }

    settings_methods!();

    /// Returns as soon as the conversion has started, without waiting for it.
    pub fn no_wait(mut self) -> Self {
        self.no_wait = true;
        self
    }
}

/// What [`Client::create_conversion`] converts: an upload or a link, to a format.
///
/// ```
/// use converter_studio::ConversionRequest;
///
/// let from_link = ConversionRequest::from_url("https://example.com/photo.heic", "jpg");
/// let from_upload = ConversionRequest::from_upload("upload-id", "docx").option("ocr", true);
/// ```
#[derive(Clone, Debug)]
#[must_use]
pub struct ConversionRequest {
    to: String,
    url: Option<String>,
    upload_id: Option<String>,
    settings: Settings,
}

impl ConversionRequest {
    /// Converts an upload ([`Client::upload_file`], [`Client::upload_bytes`]) to the format
    /// `to` (`"jpg"`, `"mp4"`, `"pdf"`...).
    pub fn from_upload(upload_id: impl Into<String>, to: impl Into<String>) -> Self {
        ConversionRequest {
            to: to.into(),
            url: None,
            upload_id: Some(upload_id.into()),
            settings: Settings::default(),
        }
    }

    /// Converts the file behind a link (a direct link, or a Google Drive, Dropbox or OneDrive
    /// share link) to the format `to`: Converter Studio fetches it itself.
    pub fn from_url(url: impl Into<String>, to: impl Into<String>) -> Self {
        ConversionRequest {
            to: to.into(),
            url: Some(url.into()),
            upload_id: None,
            settings: Settings::default(),
        }
    }

    settings_methods!();

    fn body(&self) -> Result<Value> {
        let to = normalize_format(&self.to);
        if to.is_empty() {
            return Err(no_target());
        }
        let mut body = Map::new();
        body.insert("to".to_owned(), Value::String(to));
        match (&self.url, &self.upload_id) {
            (Some(url), None) if !url.trim().is_empty() => {
                body.insert("url".to_owned(), Value::String(url.trim().to_owned()));
            }
            (None, Some(id)) if !id.is_empty() => {
                body.insert("uploadId".to_owned(), Value::String(id.clone()));
            }
            _ => {
                return Err(Error::InvalidArgument(
                    "A link or an upload id is needed.".to_owned(),
                ));
            }
        }
        let settings = &self.settings;
        if !settings.options.is_empty() {
            body.insert(
                "options".to_owned(),
                Value::Object(settings.options.clone()),
            );
        }
        if let Some(credits) = settings.max_credits {
            body.insert("maxCredits".to_owned(), json!(credits));
        }
        if let Some(url) = &settings.callback_url {
            body.insert("callbackUrl".to_owned(), Value::String(url.clone()));
        }
        Ok(Value::Object(body))
    }
}

fn normalize_format(format: &str) -> String {
    format.trim().trim_start_matches('.').to_ascii_lowercase()
}

fn no_target() -> Error {
    Error::InvalidArgument("The format to convert to is needed (such as \"jpg\").".to_owned())
}

fn valid_idempotency_key(key: &str) -> bool {
    (1..=255).contains(&key.len()) && key.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

impl Client {
    /// Starts converting an upload or a link's file (`POST /v1/conversions`) and returns at
    /// once; see [`Client::wait`]. It carries an `Idempotency-Key`, so the client's own retries
    /// never start or charge a second conversion.
    pub async fn create_conversion(&self, request: &ConversionRequest) -> Result<Conversion> {
        let body = request.body()?;
        let key = match &request.settings.idempotency_key {
            Some(key) if valid_idempotency_key(key) => key.clone(),
            Some(_) => {
                return Err(Error::InvalidArgument(
                    "The idempotency key must be 1 to 255 visible ASCII characters.".to_owned(),
                ));
            }
            None => format!("cvs-rust-{:016x}{:016x}", random_u64(), random_u64()),
        };
        let call = Call::new(Method::POST, self.api_url(&["v1", "conversions"]))
            .auth()
            .json(&body)
            .header("Idempotency-Key", key);
        let (data, headers) = self.call_json(&call).await?;
        let mut conversion = Conversion::from_json(&data)?;
        conversion.replayed = headers
            .get("Idempotent-Replayed")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("true"));
        Ok(conversion)
    }

    /// A conversion's state now (`GET /v1/conversions/{id}`).
    pub async fn conversion(&self, id: &str) -> Result<Conversion> {
        self.get_conversion(id, 0).await
    }

    /// [`Client::conversion`], but the API holds the answer until the conversion is done,
    /// failed or waits for a go-ahead, or until `max_wait` (1 to 60 seconds) has passed.
    pub async fn wait_for_update(&self, id: &str, max_wait: Duration) -> Result<Conversion> {
        self.get_conversion(id, max_wait.as_secs().clamp(1, 60))
            .await
    }

    async fn get_conversion(&self, id: &str, wait: u64) -> Result<Conversion> {
        check_id(id)?;
        let mut call = Call::new(Method::GET, self.api_url(&["v1", "conversions", id])).auth();
        if wait > 0 {
            call.url
                .query_pairs_mut()
                .append_pair("wait", &wait.to_string());
            call.hold = Duration::from_secs(wait);
        }
        let (data, _) = self.call_json(&call).await?;
        Conversion::from_json(&data)
    }

    /// Goes ahead with a conversion that waits for a go-ahead ([`Status::Quoted`]): it costs
    /// more than its maximum, or it was created with `quote_only`.
    pub async fn confirm(&self, id: &str) -> Result<Conversion> {
        check_id(id)?;
        let call = Call::new(
            Method::POST,
            self.api_url(&["v1", "conversions", id, "confirm"]),
        )
        .auth();
        let (data, _) = self.call_json(&call).await?;
        Conversion::from_json(&data)
    }

    /// Waits until the conversion is done and returns it.
    ///
    /// It fails with [`Error::ConversionFailed`] when the conversion fails, expires or is
    /// canceled, with [`Error::ConfirmationRequired`] when it waits for a go-ahead (see
    /// [`Client::confirm`]), and with [`Error::WaitTimeout`] when the wait timeout (10 minutes
    /// by default) passes first; the conversion may still finish then.
    ///
    /// The API holds each status request until something changes (up to 25 seconds), so a
    /// long conversion costs a request every 25 seconds. Answers that come back early are
    /// followed by a pause of 1 second, doubling up to 10.
    pub async fn wait(&self, id: &str) -> Result<Conversion> {
        let current = self.conversion(id).await?;
        self.wait_from(current).await
    }

    pub(crate) async fn wait_from(&self, mut current: Conversion) -> Result<Conversion> {
        let limit = self.inner.wait_timeout;
        let deadline = limit.map(|t| Instant::now() + t);
        let timed_out = |conversion: Conversion| {
            Error::WaitTimeout(Box::new(WaitTimeoutError {
                conversion,
                timeout: limit.unwrap_or_default(),
            }))
        };
        let mut pause = Duration::ZERO;
        while !current.status.is_settled() {
            let mut seconds = LONG_POLL_SECONDS;
            if let Some(deadline) = deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(timed_out(current));
                }
                seconds = remaining.as_secs().clamp(1, LONG_POLL_SECONDS);
            }
            let started = Instant::now();
            let poll = self.get_conversion(&current.id, seconds);
            let next = match deadline {
                // The API's hold plus the client's timeout may pass the deadline.
                Some(deadline) => match tokio::time::timeout_at(deadline, poll).await {
                    Ok(next) => next?,
                    Err(_) => return Err(timed_out(current)),
                },
                None => poll.await?,
            };
            current = next;
            if current.status.is_settled() {
                break;
            }
            if started.elapsed() < Duration::from_secs(seconds) / 2 {
                pause = if pause.is_zero() {
                    Duration::from_secs(1)
                } else {
                    (pause * 2).min(Duration::from_secs(10))
                };
                let mut sleep = pause;
                if let Some(deadline) = deadline {
                    sleep = sleep.min(deadline.saturating_duration_since(Instant::now()));
                }
                self.pause(sleep).await;
            } else {
                pause = Duration::ZERO;
            }
        }
        settled(current)
    }

    /// Uploads a local file, converts it to the format `to` (`"jpg"`, `"mp4"`, `"pdf"`...)
    /// and, unless [`no_wait`](ConvertParams::no_wait), waits until it is done (see
    /// [`Client::wait`]). Save the result with [`Client::download_file`].
    ///
    /// ```no_run
    /// # async fn run() -> converter_studio::Result<()> {
    /// use converter_studio::{Client, ConvertParams};
    ///
    /// let client = Client::new()?;
    /// let params = ConvertParams::new().option("height", "720");
    /// let conversion = client.convert_file("talk.mov", "mp4", params).await?;
    /// let path = client.download_file(&conversion, "out/").await?; // out/talk.mp4
    /// # Ok(()) }
    /// ```
    pub async fn convert_file(
        &self,
        path: impl AsRef<Path>,
        to: &str,
        params: ConvertParams,
    ) -> Result<Conversion> {
        if normalize_format(to).is_empty() {
            return Err(no_target());
        }
        let upload = self.upload_file(path).await?;
        self.convert(ConversionRequest::from_upload(upload.id, to), params)
            .await
    }

    /// [`Client::convert_file`] for bytes in memory, as a file named `file_name` (the API
    /// reads the format from the bytes and the name).
    pub async fn convert_bytes(
        &self,
        bytes: impl Into<Bytes>,
        file_name: &str,
        to: &str,
        params: ConvertParams,
    ) -> Result<Conversion> {
        if normalize_format(to).is_empty() {
            return Err(no_target());
        }
        let upload = self.upload_bytes(bytes, file_name).await?;
        self.convert(ConversionRequest::from_upload(upload.id, to), params)
            .await
    }

    /// Converts the file behind a link (a direct link, or a Google Drive, Dropbox or OneDrive
    /// share link): Converter Studio fetches it itself, so it never passes through this
    /// machine. Otherwise it is [`Client::convert_file`].
    pub async fn convert_url(
        &self,
        url: &str,
        to: &str,
        params: ConvertParams,
    ) -> Result<Conversion> {
        self.convert(ConversionRequest::from_url(url, to), params)
            .await
    }

    async fn convert(
        &self,
        mut request: ConversionRequest,
        params: ConvertParams,
    ) -> Result<Conversion> {
        request.settings = params.settings;
        let created = self.create_conversion(&request).await?;
        if params.no_wait {
            return Ok(created);
        }
        self.wait_from(created).await
    }
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(Error::InvalidArgument(
            "A conversion id is needed.".to_owned(),
        ));
    }
    Ok(())
}

/// The conversion when it is done, and the error when it ended without a result or waits for
/// a go-ahead.
pub(crate) fn settled(conversion: Conversion) -> Result<Conversion> {
    match conversion.status {
        Status::Done => Ok(conversion),
        Status::Quoted => Err(Error::ConfirmationRequired(Box::new(
            ConfirmationRequiredError::new(conversion),
        ))),
        status if status.is_finished() => Err(Error::ConversionFailed(Box::new(
            ConversionFailedError::new(conversion),
        ))),
        _ => Err(Error::NotDone(Box::new(conversion))),
    }
}
