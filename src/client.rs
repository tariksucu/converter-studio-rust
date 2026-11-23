//! The client and its builder.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;

use crate::error::{Error, Result};

/// The address of the Converter Studio API.
pub const DEFAULT_BASE_URL: &str = "https://converter.studio/api";
/// The environment variable the API key is read from when the builder is given none.
pub const API_KEY_ENV: &str = "CONVERTER_STUDIO_API_KEY";
/// The environment variable the API's address is read from when the builder is given none.
pub const BASE_URL_ENV: &str = "CONVERTER_STUDIO_BASE_URL";
/// How long a request may go without progress by default ([`ClientBuilder::timeout`]).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// How often a request is sent again by default ([`ClientBuilder::max_retries`]).
pub const DEFAULT_MAX_RETRIES: u32 = 2;
/// How long [`Client::wait`] waits for a conversion by default
/// ([`ClientBuilder::wait_timeout`]).
pub const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The version of this crate. It is sent with every request in the
/// `X-Converter-Studio-Client` header (`rust/<version>`), for usage statistics only.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) const CLIENT_NAME: &str = "rust";
pub(crate) const CLIENT_HEADER: &str = "X-Converter-Studio-Client";
/// One request's body at most (the CDN in front of the API stops at 100 MB).
pub(crate) const DEFAULT_PART_BYTES: u64 = 95 << 20;

type SleepFn = dyn Fn(Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync;

/// A client for the Converter Studio API.
///
/// Create one with [`Client::new`] (the API key from `CONVERTER_STUDIO_API_KEY`) or
/// [`Client::builder`]. It is cheap to clone (the clones share one connection pool) and safe to
/// use from several tasks at once.
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) api_key: Option<String>,
    /// The API's address, without a trailing slash.
    pub(crate) base_url: Url,
    pub(crate) http: reqwest::Client,
    pub(crate) user_agent: String,
    pub(crate) timeout: Duration,
    pub(crate) max_retries: u32,
    pub(crate) wait_timeout: Option<Duration>,
    pub(crate) part_bytes: Option<u64>,
    /// Pauses between attempts and between status requests; the tests replace it.
    pub(crate) sleep: Arc<SleepFn>,
}

impl Client {
    /// A client with the default settings: the API key from `CONVERTER_STUDIO_API_KEY`, the
    /// API's address from `CONVERTER_STUDIO_BASE_URL` or else <https://converter.studio/api>.
    ///
    /// It fails only on an invalid `CONVERTER_STUDIO_BASE_URL`. A missing API key is reported
    /// by the calls that need one, as [`Error::MissingApiKey`]; listing formats needs no key.
    pub fn new() -> Result<Client> {
        ClientBuilder::new().build()
    }

    /// A builder to set the API key, the address, timeouts and retries.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }

    /// The API's address the client talks to.
    pub fn base_url(&self) -> &str {
        self.inner.base_url.as_str().trim_end_matches('/')
    }

    /// An address under the API's, from path segments (escaped).
    pub(crate) fn api_url(&self, segments: &[&str]) -> Url {
        let mut url = self.inner.base_url.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty().extend(segments);
        }
        url
    }

    pub(crate) async fn pause(&self, d: Duration) {
        if !d.is_zero() {
            (self.inner.sleep)(d).await;
        }
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = &self.inner;
        f.debug_struct("Client")
            .field("base_url", &self.base_url())
            .field("api_key", &inner.api_key.as_ref().map(|_| "<redacted>"))
            .field("timeout", &inner.timeout)
            .field("max_retries", &inner.max_retries)
            .field("wait_timeout", &inner.wait_timeout)
            .finish_non_exhaustive()
    }
}

/// Settings of a [`Client`].
///
/// ```no_run
/// use std::time::Duration;
///
/// let client = converter_studio::Client::builder()
///     .api_key("cvs_live_...") // default: CONVERTER_STUDIO_API_KEY
///     .timeout(Duration::from_secs(60)) // a request without progress
///     .max_retries(2) // network errors, 408, 429, 5xx
///     .wait_timeout(Duration::from_secs(600)) // how long `wait` waits
///     .user_agent("my-app/1.2")
///     .build()?;
/// # Ok::<(), converter_studio::Error>(())
/// ```
#[must_use]
pub struct ClientBuilder {
    api_key: Option<String>,
    base_url: Option<String>,
    http: Option<reqwest::Client>,
    user_agent: Option<String>,
    timeout: Duration,
    max_retries: u32,
    wait_timeout: Option<Duration>,
    part_bytes: Option<u64>,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        ClientBuilder::new()
    }
}

impl fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url)
            .field("timeout", &self.timeout)
            .field("max_retries", &self.max_retries)
            .field("wait_timeout", &self.wait_timeout)
            .finish_non_exhaustive()
    }
}

impl ClientBuilder {
    /// A builder with the default settings.
    pub fn new() -> ClientBuilder {
        ClientBuilder {
            api_key: None,
            base_url: None,
            http: None,
            user_agent: None,
            timeout: DEFAULT_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            wait_timeout: Some(DEFAULT_WAIT_TIMEOUT),
            part_bytes: None,
        }
    }

    /// The account's API key (keys start with `cvs_live_`; create one at
    /// <https://converter.studio/account#api-keys>). Without it, the key is read from
    /// `CONVERTER_STUDIO_API_KEY`.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into().trim().to_owned());
        self
    }

    /// The API's address. Without it, it is read from `CONVERTER_STUDIO_BASE_URL`, or else it
    /// is [`DEFAULT_BASE_URL`].
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Send the requests with your own `reqwest` client (for a proxy, your own certificates or
    /// a shared connection pool). It should have no overall timeout: the API holds status
    /// requests for up to a minute, and large files take longer than that; use
    /// [`timeout`](Self::timeout) instead.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http = Some(client);
        self
    }

    /// Your application's name and version (such as `"my-app/1.2"`), put in front of the
    /// client's own `User-Agent` header.
    pub fn user_agent(mut self, product: impl Into<String>) -> Self {
        self.user_agent = Some(product.into());
        self
    }

    /// How long a request may go without progress: to connect and get an answer, or between
    /// two pieces of a file sent or received ([`DEFAULT_TIMEOUT`] by default). It is not a
    /// limit on a whole call, which may send or receive a large file.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// How often a request is sent again after a network error or a 408, 429 or 5xx answer
    /// ([`DEFAULT_MAX_RETRIES`] by default; 0 turns retries off). The pauses grow (half a
    /// second, doubling up to 8 seconds) or follow the API's `Retry-After`. Conversions are
    /// created with an `Idempotency-Key`, so a retry never starts or charges a second one.
    pub fn max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// How long [`Client::wait`] and the `convert_*` methods wait for a conversion to finish
    /// ([`DEFAULT_WAIT_TIMEOUT`] by default; `None`: as long as it takes).
    pub fn wait_timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.wait_timeout = timeout.into();
        self
    }

    /// Send files in parts of at most this many bytes. By default a part is as large as the
    /// API allows (95 MB), so only larger files go in parts.
    pub fn part_size(mut self, bytes: u64) -> Self {
        self.part_bytes = Some(bytes.max(1));
        self
    }

    /// Builds the client.
    ///
    /// It fails on an invalid base URL or user agent, a timeout of zero, or when the HTTP
    /// client cannot be set up.
    pub fn build(self) -> Result<Client> {
        let base_url = match self.base_url {
            Some(url) => check_base_url(&url)?,
            None => match std::env::var(BASE_URL_ENV) {
                Ok(url) if !url.trim().is_empty() => check_base_url(&url)
                    .map_err(|e| Error::Config(format!("{e} (from {BASE_URL_ENV})")))?,
                _ => check_base_url(DEFAULT_BASE_URL)?,
            },
        };
        let api_key = self
            .api_key
            .or_else(|| std::env::var(API_KEY_ENV).ok())
            .map(|key| key.trim().to_owned())
            .filter(|key| !key.is_empty());
        if self.timeout.is_zero() {
            return Err(Error::Config("The timeout must be more than 0.".to_owned()));
        }
        let own_agent = format!("converter-studio-rust/{VERSION}");
        let user_agent = match self.user_agent.as_deref().map(str::trim) {
            Some(product) if product.contains(['\r', '\n']) => {
                return Err(Error::Config(
                    "The user agent must be on one line.".to_owned(),
                ));
            }
            Some(product) if !product.is_empty() => format!("{product} {own_agent}"),
            _ => own_agent,
        };
        if reqwest::header::HeaderValue::from_str(&user_agent).is_err() {
            return Err(Error::Config(
                "The user agent must be visible ASCII text.".to_owned(),
            ));
        }
        let http = match self.http {
            Some(client) => client,
            None => reqwest::Client::builder()
                .connect_timeout(self.timeout)
                .build()
                .map_err(|e| Error::Config(format!("The HTTP client cannot be set up: {e}")))?,
        };
        Ok(Client {
            inner: Arc::new(Inner {
                api_key,
                base_url,
                http,
                user_agent,
                timeout: self.timeout,
                max_retries: self.max_retries,
                wait_timeout: self.wait_timeout.filter(|t| !t.is_zero()),
                part_bytes: self.part_bytes,
                sleep: Arc::new(|d| Box::pin(tokio::time::sleep(d))),
            }),
        })
    }
}

fn check_base_url(raw: &str) -> Result<Url> {
    let trimmed = raw.trim().trim_end_matches('/');
    let invalid = || {
        Error::Config(format!(
            "The base URL must be an http:// or https:// address, not {raw:?}."
        ))
    };
    let url = Url::parse(trimmed).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(invalid());
    }
    if url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Error::Config(
            "The base URL must not hold a query, a fragment, a user name or a password.".to_owned(),
        ));
    }
    Ok(url)
}

#[cfg(test)]
impl Client {
    /// Replaces the pauses with `sleep` (the tests record them instead of waiting).
    pub(crate) fn with_sleep<F>(mut self, sleep: F) -> Client
    where
        F: Fn(Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
    {
        let inner = Arc::get_mut(&mut self.inner).expect("a client that is not shared yet");
        inner.sleep = Arc::new(sleep);
        self
    }
}
