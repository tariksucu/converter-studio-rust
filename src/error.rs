//! The errors of this crate: [`Error`] and the details it carries.

use std::fmt;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::models::{Conversion, Status};

/// A `Result` whose error is this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong in a call to the Converter Studio API.
///
/// For the API's own errors and for conversions that failed or wait, the error's text (its
/// [`Display`](fmt::Display)) is a sentence meant to be shown to people: the API's own message.
///
/// ```no_run
/// # async fn run(client: converter_studio::Client) {
/// use converter_studio::{ApiErrorKind, ConvertParams, Error};
///
/// match client.convert_file("broken.heic", "jpg", ConvertParams::default()).await {
///     Ok(conversion) => println!("done: {:?}", conversion.output_name),
///     Err(Error::ConversionFailed(failed)) => println!("{}: {}", failed.code, failed.message),
///     Err(e) if e.api_kind() == Some(ApiErrorKind::InsufficientCredits) => {
///         println!("Buy credits at https://converter.studio/pricing");
///     }
///     Err(e) => println!("{e}"),
/// }
/// # }
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// No API key was given and `CONVERTER_STUDIO_API_KEY` is not set. Create a key at
    /// <https://converter.studio/account#api-keys>.
    MissingApiKey,
    /// The API answered with an error: a wrong key, not enough credits, a format it cannot
    /// convert to... See [`ApiError::kind`].
    Api(Box<ApiError>),
    /// The conversion failed, expired or was canceled. Nothing is charged for a failure.
    ConversionFailed(Box<ConversionFailedError>),
    /// The conversion costs more than its maximum (20 credits by default) and waits for
    /// [`Client::confirm`](crate::Client::confirm). Nothing has been charged.
    ConfirmationRequired(Box<ConfirmationRequiredError>),
    /// The conversion did not finish within the wait timeout. It may still finish: look it up
    /// later by its id with [`Client::conversion`](crate::Client::conversion).
    WaitTimeout(Box<WaitTimeoutError>),
    /// A download was asked for a conversion that is not done yet: wait for it first.
    NotDone(Box<Conversion>),
    /// The API (or an upload or download link) could not be reached, or the connection broke,
    /// after the retries.
    Connection(Box<ConnectionError>),
    /// An argument of a call was not right (no target format, both a link and an upload...).
    InvalidArgument(String),
    /// The client could not be set up (an invalid base URL or user agent).
    Config(String),
    /// A local file could not be read or written.
    Io(std::io::Error),
    /// The API sent an answer this client cannot read.
    InvalidResponse(String),
}

impl Error {
    /// The kind of the API's error, if this is one.
    pub fn api_kind(&self) -> Option<ApiErrorKind> {
        match self {
            Error::Api(e) => Some(e.kind()),
            _ => None,
        }
    }

    /// The API's stable error code (`"invalidApiKey"`, `"notEnoughCredits"`,
    /// `"conversionFailed"`...), for an error answer of the API or a failed conversion.
    pub fn code(&self) -> Option<&str> {
        match self {
            Error::Api(e) => e.code.as_deref(),
            Error::ConversionFailed(e) => Some(&e.code),
            _ => None,
        }
    }

    /// The conversion this error is about, in its last known state: one that failed, waits
    /// for a go-ahead, did not finish in time or is not done yet.
    pub fn conversion(&self) -> Option<&Conversion> {
        match self {
            Error::ConversionFailed(e) => Some(&e.conversion),
            Error::ConfirmationRequired(e) => Some(&e.conversion),
            Error::WaitTimeout(e) => Some(&e.conversion),
            Error::NotDone(c) => Some(c),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::MissingApiKey => f.write_str(
                "No API key: pass one to the client builder or set CONVERTER_STUDIO_API_KEY \
                 (create a key at https://converter.studio/account#api-keys).",
            ),
            Error::Api(e) => e.fmt(f),
            Error::ConversionFailed(e) => e.fmt(f),
            Error::ConfirmationRequired(e) => e.fmt(f),
            Error::WaitTimeout(e) => e.fmt(f),
            Error::NotDone(c) => write!(
                f,
                "The conversion is not done yet (it is {}); wait for it first.",
                c.status
            ),
            Error::Connection(e) => e.fmt(f),
            Error::InvalidArgument(message) | Error::Config(message) => f.write_str(message),
            Error::Io(e) => e.fmt(f),
            Error::InvalidResponse(message) => {
                write!(
                    f,
                    "Unreadable answer from the Converter Studio API: {message}"
                )
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            // A connection error's message already holds its cause (see ConnectionError::cause).
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// What an [`ApiError`] is about, from its HTTP status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ApiErrorKind {
    /// 400: the request was not right (`invalidRequest`, `invalidUrl`, `invalidCallbackUrl`,
    /// `invalidIdempotencyKey`...).
    BadRequest,
    /// 401: the API key does not work (wrong or deleted).
    Authentication,
    /// 402: not enough credits (params `needed`, `balance`).
    InsufficientCredits,
    /// 403: the plan does not include this.
    PermissionDenied,
    /// 404: no such conversion or upload on this account.
    NotFound,
    /// 409: the request conflicts with what is there (`idempotencyKeyReused`...).
    Conflict,
    /// 410: the result has been deleted (`fileExpired`).
    Gone,
    /// 413: the file is larger than the plan allows (param `maxSize`).
    FileTooLarge,
    /// 422: the file cannot be converted this way (`unsupportedConversion`).
    Unprocessable,
    /// 429: too many requests; retried on their own first.
    RateLimited,
    /// 5xx: the service is busy or failed (`serverBusy`); retried on their own first.
    Server,
    /// Any other status.
    Other,
}

/// An error answer of the API.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ApiError {
    /// The HTTP status.
    pub status: u16,
    /// The API's stable error code (`"invalidApiKey"`, `"notEnoughCredits"`,
    /// `"unsupportedConversion"`...). `None` only when the answer did not come from the API
    /// itself (a proxy's error page).
    pub code: Option<String>,
    /// The API's English sentence for people.
    pub message: String,
    /// The values in the message (`needed`, `balance`, `maxSize`...).
    pub params: Map<String, Value>,
    /// How long the API asked to wait before trying again (429, 503).
    pub retry_after: Option<Duration>,
}

impl ApiError {
    /// What the error is about, from its HTTP status.
    pub fn kind(&self) -> ApiErrorKind {
        match self.status {
            400 => ApiErrorKind::BadRequest,
            401 => ApiErrorKind::Authentication,
            402 => ApiErrorKind::InsufficientCredits,
            403 => ApiErrorKind::PermissionDenied,
            404 => ApiErrorKind::NotFound,
            409 => ApiErrorKind::Conflict,
            410 => ApiErrorKind::Gone,
            413 => ApiErrorKind::FileTooLarge,
            422 => ApiErrorKind::Unprocessable,
            429 => ApiErrorKind::RateLimited,
            500..=599 => ApiErrorKind::Server,
            _ => ApiErrorKind::Other,
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// A conversion that failed, expired or was canceled. Nothing is charged for a failure.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ConversionFailedError {
    /// The API's code (`"conversionFailed"`, `"noText"`, `"wrongPassword"`,
    /// `"remoteNotFound"`...), or `"fileExpired"` and `"canceled"`.
    pub code: String,
    /// A sentence for people.
    pub message: String,
    /// The values in the message.
    pub params: Map<String, Value>,
    /// The conversion in its last known state.
    pub conversion: Conversion,
}

impl ConversionFailedError {
    pub(crate) fn new(conversion: Conversion) -> Self {
        let (code, message, params) = match (&conversion.error, conversion.status) {
            (Some(info), _) if !info.code.is_empty() => (
                info.code.clone(),
                if info.message.is_empty() {
                    info.code.clone()
                } else {
                    info.message.clone()
                },
                info.params.clone(),
            ),
            (_, Status::Expired) => (
                "fileExpired".to_owned(),
                "The result was deleted after its time ran out.".to_owned(),
                Map::new(),
            ),
            (_, Status::Canceled) => (
                "canceled".to_owned(),
                "The conversion was canceled; nothing was charged.".to_owned(),
                Map::new(),
            ),
            _ => (
                "conversionFailed".to_owned(),
                if conversion.next.is_empty() {
                    "The conversion failed.".to_owned()
                } else {
                    conversion.next.clone()
                },
                Map::new(),
            ),
        };
        ConversionFailedError {
            code,
            message,
            params,
            conversion,
        }
    }
}

impl fmt::Display for ConversionFailedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConversionFailedError {}

/// A conversion that costs more than its maximum (20 credits by default): it waits for
/// [`Client::confirm`](crate::Client::confirm) and nothing has been charged.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ConfirmationRequiredError {
    /// The price in credits, if the API said.
    pub credits: Option<u32>,
    /// The price that would have gone ahead on its own (0 with
    /// [`quote_only`](crate::ConvertParams::quote_only)).
    pub max_credits: u32,
    /// The conversion that waits ([`Status::Quoted`]).
    pub conversion: Conversion,
}

impl ConfirmationRequiredError {
    pub(crate) fn new(conversion: Conversion) -> Self {
        ConfirmationRequiredError {
            credits: conversion.credits,
            max_credits: conversion.max_credits.unwrap_or(0),
            conversion,
        }
    }
}

impl fmt::Display for ConfirmationRequiredError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.credits {
            None => f.write_str(
                "This conversion waits for your go-ahead. Confirm it to start, or leave it: \
                 nothing has been charged.",
            ),
            Some(credits) => write!(
                f,
                "This conversion costs {credits} {}, more than the {} allowed to go ahead on \
                 their own. Confirm it to start, or leave it: nothing has been charged.",
                if credits == 1 { "credit" } else { "credits" },
                self.max_credits
            ),
        }
    }
}

impl std::error::Error for ConfirmationRequiredError {}

/// A conversion that did not finish within the wait timeout. It may still finish: look it up
/// later by its id with [`Client::conversion`](crate::Client::conversion).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct WaitTimeoutError {
    /// The conversion in its last known state.
    pub conversion: Conversion,
    /// The wait timeout that passed.
    pub timeout: Duration,
}

impl fmt::Display for WaitTimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "The conversion did not finish within {} s (it is {}). It may still finish: look it \
             up later by its id, {}.",
            self.timeout.as_secs_f64(),
            self.conversion.status,
            self.conversion.id
        )
    }
}

impl std::error::Error for WaitTimeoutError {}

/// The API (or an upload or download link) could not be reached, or the connection broke,
/// after the retries. It names only the host: upload and download links carry tokens.
#[derive(Debug)]
#[non_exhaustive]
pub struct ConnectionError {
    /// The host that could not be reached.
    pub host: String,
    /// Whether the request went without progress for longer than the client's timeout.
    pub timeout: bool,
    pub(crate) source: Option<Box<dyn std::error::Error + Send + Sync>>,
    pub(crate) retryable: bool,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.timeout {
            return write!(f, "The request to {} timed out.", self.host);
        }
        write!(f, "Could not reach {}", self.host)?;
        if let Some(source) = &self.source {
            write!(f, ": {source}")?;
            let mut inner = source.source();
            while let Some(cause) = inner {
                write!(f, ": {cause}")?;
                inner = cause.source();
            }
        }
        f.write_str(".")
    }
}

impl ConnectionError {
    /// The network error underneath (its text is already part of this error's message).
    pub fn cause(&self) -> Option<&(dyn std::error::Error + Send + Sync + 'static)> {
        self.source.as_deref()
    }
}

impl std::error::Error for ConnectionError {}
