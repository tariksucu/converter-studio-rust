//! What the API returns: conversions, uploads, formats and the account.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Where a conversion stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Status {
    /// Converter Studio is downloading the file from its link.
    Fetching,
    /// The upload has not received its file yet.
    WaitingForUpload,
    /// Audio and video are measured to price them.
    Measuring,
    /// It costs more than its maximum and waits for [`Client::confirm`](crate::Client::confirm).
    Quoted,
    /// It waits for a free worker.
    Queued,
    /// It is being converted.
    Processing,
    /// The result is ready to download.
    Done,
    /// It failed; nothing was charged.
    Failed,
    /// The result was deleted after its time ran out.
    Expired,
    /// It was canceled; nothing was charged.
    Canceled,
    /// A status this version of the client does not know yet.
    #[default]
    #[serde(other)]
    Unknown,
}

impl Status {
    /// The API's name for the status (`"done"`, `"waiting_for_upload"`...).
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Fetching => "fetching",
            Status::WaitingForUpload => "waiting_for_upload",
            Status::Measuring => "measuring",
            Status::Quoted => "quoted",
            Status::Queued => "queued",
            Status::Processing => "processing",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Expired => "expired",
            Status::Canceled => "canceled",
            Status::Unknown => "unknown",
        }
    }

    /// Whether a conversion in this status will not change any more: done, failed, expired or
    /// canceled.
    pub fn is_finished(self) -> bool {
        matches!(
            self,
            Status::Done | Status::Failed | Status::Expired | Status::Canceled
        )
    }

    /// Whether nobody needs to wait any longer: finished, or waiting for a go-ahead.
    pub(crate) fn is_settled(self) -> bool {
        self.is_finished() || self == Status::Quoted
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a conversion failed, as the API says it: a stable code and an English message meant to
/// be shown to people.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ErrorInfo {
    /// The API's stable error code: `"conversionFailed"`, `"noText"`, `"wrongPassword"`,
    /// `"remoteNotFound"`...
    #[serde(default)]
    pub code: String,
    /// A sentence for people.
    #[serde(default)]
    pub message: String,
    /// The values in the message, if any.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
}

/// A conversion as the API returns it (`GET /v1/conversions/{id}`). Its id is also the id of
/// its upload.
///
/// Its [`Debug`](fmt::Debug) output leaves out the download link, which works for anyone who
/// has it, so that printing a conversion never puts the link in a log.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Conversion {
    /// The conversion's id.
    pub id: String,
    /// Where it stands.
    #[serde(default)]
    pub status: Status,
    /// The format converted from (such as `"heic"`), once known.
    #[serde(default)]
    pub from: Option<String>,
    /// The format converted to (such as `"jpg"`).
    #[serde(default)]
    pub to: Option<String>,
    /// The name of the file that is converted.
    #[serde(default)]
    pub input_name: Option<String>,
    /// The size of the file that is converted, in bytes.
    #[serde(default)]
    pub input_size: Option<u64>,
    /// The price in credits, once known (audio and video are priced once they have been
    /// measured).
    #[serde(default)]
    pub credits: Option<u32>,
    /// The price up to which it goes ahead on its own; `None`: it waits for a go-ahead
    /// ([`Client::confirm`](crate::Client::confirm)).
    #[serde(default)]
    pub max_credits: Option<u32>,
    /// From 0 to 1 while it converts.
    #[serde(default)]
    pub progress: Option<f64>,
    /// The result's file name, once it is done.
    #[serde(default)]
    pub output_name: Option<String>,
    /// The result's size in bytes, once it is done.
    #[serde(default)]
    pub output_size: Option<u64>,
    /// Where the result is downloaded from. Anyone with this link can download the result
    /// until [`expires_at`](Self::expires_at), so keep it out of logs.
    #[serde(default)]
    pub download_url: Option<String>,
    /// When the result is deleted (RFC 3339), 6 hours after the conversion.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Why it failed ([`Status::Failed`] only).
    #[serde(default)]
    pub error: Option<ErrorInfo>,
    /// What to do next, in a sentence.
    #[serde(default)]
    pub next: String,
    /// Whether the API answered a repeated request (the same `Idempotency-Key`) with this
    /// conversion instead of starting a second one.
    #[serde(skip)]
    pub replayed: bool,
    /// The JSON the API sent, for fields this version of the client does not name yet.
    #[serde(skip)]
    pub raw: Value,
}

impl Conversion {
    /// Whether the result is ready to download.
    pub fn is_done(&self) -> bool {
        self.status == Status::Done
    }

    /// Whether the conversion will not change any more: done, failed, expired or canceled.
    pub fn is_finished(&self) -> bool {
        self.status.is_finished()
    }

    /// Whether it costs more than its maximum and waits for
    /// [`Client::confirm`](crate::Client::confirm).
    pub fn needs_confirmation(&self) -> bool {
        self.status == Status::Quoted
    }

    pub(crate) fn from_json(data: &[u8]) -> Result<Conversion, crate::Error> {
        let raw: Value = serde_json::from_slice(data)
            .map_err(|e| crate::Error::InvalidResponse(e.to_string()))?;
        let mut conversion: Conversion = serde_json::from_value(raw.clone())
            .map_err(|e| crate::Error::InvalidResponse(e.to_string()))?;
        conversion.raw = raw;
        Ok(conversion)
    }
}

impl fmt::Debug for Conversion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Conversion")
            .field("id", &self.id)
            .field("status", &self.status)
            .field("from", &self.from)
            .field("to", &self.to)
            .field("input_name", &self.input_name)
            .field("credits", &self.credits)
            .field("output_name", &self.output_name)
            .field("output_size", &self.output_size)
            .field(
                "download_url",
                &self.download_url.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

/// An upload slot (`POST /v1/uploads`). The file is sent to its link;
/// [`Client::upload_file`](crate::Client::upload_file) does it for you.
///
/// Its [`Debug`](fmt::Debug) output leaves out the link, which carries the upload's token.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Upload {
    /// The upload's id, which is also the id of its conversion.
    pub id: String,
    /// Where the file is sent. It carries the upload's own token: it needs no API key.
    pub upload_url: String,
    /// The HTTP method (`PUT`).
    #[serde(default)]
    pub method: String,
    /// The largest file the account's plan accepts, in bytes.
    #[serde(default)]
    pub max_bytes: u64,
    /// The most one request carries; larger files go in parts.
    #[serde(default)]
    pub part_bytes: u64,
    /// When the slot is deleted if nothing arrives (RFC 3339).
    #[serde(default)]
    pub expires_at: Option<String>,
}

impl fmt::Debug for Upload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Upload")
            .field("id", &self.id)
            .field("upload_url", &"<redacted>")
            .field("max_bytes", &self.max_bytes)
            .field("part_bytes", &self.part_bytes)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// A format the API reads, and what it converts it to right now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Format {
    /// The format's id (`"heic"`, `"mp4"`, `"docx"`...), also used as a target.
    pub id: String,
    /// Its name for people (`"HEIC"`).
    #[serde(default)]
    pub name: String,
    /// Its category (`"image"`, `"video"`, `"document"`...).
    #[serde(default)]
    pub category: String,
    /// The file extensions it goes by.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// The formats it converts to right now, the most popular first.
    #[serde(rename = "to", default)]
    pub targets: Vec<String>,
}

/// The plan's credits this period.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Allowance {
    /// Credits used this period.
    #[serde(default)]
    pub used: i64,
    /// Credits the plan gives each period.
    #[serde(default)]
    pub limit: i64,
    /// `"day"` or `"month"`.
    #[serde(default)]
    pub per: String,
}

/// The web wallet, which pays for cloud work past the credits when allowed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Wallet {
    /// The balance in millionths of a US dollar.
    #[serde(default)]
    pub balance_micros: i64,
    /// Whether it pays once the credits run out.
    #[serde(default)]
    pub pays: bool,
}

/// The account of the API key (`GET /v1/account`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Account {
    /// The account's e-mail address.
    #[serde(default)]
    pub email: String,
    /// The plan: `"free"`, `"pro"`, `"premium"`...
    #[serde(default)]
    pub plan: String,
    /// Converter Studio on every platform (the app plan, Pro or Premium).
    #[serde(default)]
    pub all_apps: bool,
    /// Credits left: the plan's and the packs'.
    #[serde(default)]
    pub credits: i64,
    /// The plan's credits this period.
    #[serde(default)]
    pub allowance: Option<Allowance>,
    /// Credits left in packs (they never expire).
    #[serde(default)]
    pub packs: i64,
    /// When the plan's credits renew (RFC 3339).
    #[serde(default)]
    pub renews_at: Option<String>,
    /// The largest file the plan accepts, in bytes.
    #[serde(default)]
    pub max_file_bytes: u64,
    /// The longest audio or video the plan accepts, in seconds.
    #[serde(default)]
    pub max_media_seconds: u64,
    /// The web wallet, if the account has one.
    #[serde(default)]
    pub wallet: Option<Wallet>,
    /// The JSON the API sent, for fields this version of the client does not name yet.
    #[serde(skip)]
    pub raw: Value,
}
