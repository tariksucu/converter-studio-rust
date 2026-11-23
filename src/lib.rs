//! The official Rust client for the [Converter Studio] API.
//!
//! Converter Studio converts iPhone photos (HEIC), camera RAW photos, videos, audio,
//! documents, spreadsheets, presentations, e-books, subtitles, fonts, data files and archives
//! (67 formats). The conversion runs on Converter Studio's servers, so there is nothing else to
//! install: no ImageMagick, no FFmpeg, no LibreOffice, no C libraries to link.
//!
//! ```no_run
//! use converter_studio::{Client, ConvertParams};
//!
//! # async fn run() -> converter_studio::Result<()> {
//! let client = Client::new()?; // the API key comes from CONVERTER_STUDIO_API_KEY
//! let conversion = client
//!     .convert_file("IMG_1234.HEIC", "jpg", ConvertParams::default())
//!     .await?;
//! let path = client.download_file(&conversion, "photos/").await?; // photos/IMG_1234.jpg
//! # Ok(()) }
//! ```
//!
//! The client is async (Tokio and reqwest with rustls). With the `blocking` feature,
//! [`blocking::Client`] offers the same calls without async.
//!
//! # API key and credits
//!
//! Create an API key on your [account page] (keys start with `cvs_live_`; keep them secret)
//! and put it in the `CONVERTER_STUDIO_API_KEY` environment variable, or pass it with
//! [`ClientBuilder::api_key`]. Listing formats needs no key. Every conversion uses the
//! account's credits, the same as on the website: most files cost 1 credit; audio and video are
//! priced by their length once Converter Studio has looked at them. See [pricing].
//!
//! # Converting
//!
//! [`Client::convert_file`] (a local file), [`Client::convert_bytes`] (bytes in memory) and
//! [`Client::convert_url`] (a link, which Converter Studio fetches itself) upload the file,
//! start the conversion and wait until it is done. [`ConvertParams`] takes the API's conversion
//! settings, such as `option("quality", "high")` for video and JPG, `option("height", "720")`
//! for video, `option("keepMetadata", true)` for photos, `option("pageRange", "1-3, 5")` for
//! documents to PDF or `option("ocr", true)` for scanned PDFs to Word; the full list is in the
//! [API documentation]. Save the result with [`Client::download_file`], stream it to any
//! writer with [`Client::download`] or keep it in memory with [`Client::download_bytes`].
//! Results are kept for 6 hours.
//!
//! The steps can also be taken one by one: [`Client::upload_file`] (large files go in parts on
//! their own), [`Client::create_conversion`], [`Client::conversion`] and [`Client::wait`],
//! which lets the API hold each status request until something changes (long polling) and
//! pauses with a back-off between answers that come back early.
//!
//! # Prices, callbacks and retries
//!
//! A conversion goes ahead on its own when it costs at most its maximum
//! ([`ConvertParams::max_credits`], 20 credits by default). A dearer one fails with
//! [`Error::ConfirmationRequired`], costs nothing, and goes ahead with [`Client::confirm`].
//! [`ConvertParams::quote_only`] only gets the price.
//!
//! [`ConvertParams::callback_url`] asks Converter Studio to POST to your server when a
//! conversion waits for a go-ahead and when it ends. Requests that fail on the network or with
//! a 408, 429 or 5xx answer are sent again ([`ClientBuilder::max_retries`]); conversions are
//! created with an `Idempotency-Key`, so a retry never starts or charges a second one.
//!
//! # Errors
//!
//! Every call returns this crate's [`Error`]. The API's errors are [`Error::Api`], carrying the
//! API's code (`"notEnoughCredits"`...) and its message, a sentence meant for people;
//! [`Error::api_kind`] tells them apart by status. A failed conversion is
//! [`Error::ConversionFailed`], one that did not finish in time [`Error::WaitTimeout`], and an
//! unreachable API [`Error::Connection`].
//!
//! # Features
//!
//! - `blocking`: [`blocking::Client`], the same calls without async (it runs its own Tokio
//!   runtime).
//! - `cli`: the `converter-studio` command (`cargo install converter-studio --features cli`).
//!
//! No feature is on by default. The minimum supported Rust version is 1.85.
//!
//! # Privacy
//!
//! Files are sent to Converter Studio (converter.studio) to be converted. Uploaded files are
//! deleted as soon as they are converted, and results 6 hours later. Each request says which
//! client sent it (`X-Converter-Studio-Client: rust/<version>`), for usage statistics only.
//! The API key is sent to the API's address only; upload and download links carry their own
//! tokens. See the [privacy policy].
//!
//! [Converter Studio]: https://converter.studio
//! [API documentation]: https://converter.studio/developers
//! [account page]: https://converter.studio/account#api-keys
//! [pricing]: https://converter.studio/pricing
//! [privacy policy]: https://converter.studio/privacy
#![cfg_attr(docsrs, feature(doc_cfg))]

mod account;
#[cfg(feature = "blocking")]
#[cfg_attr(docsrs, doc(cfg(feature = "blocking")))]
pub mod blocking;
mod client;
mod conversions;
mod download;
mod error;
mod models;
mod request;
mod uploads;

#[cfg(test)]
mod tests;

pub use client::{
    API_KEY_ENV, BASE_URL_ENV, Client, ClientBuilder, DEFAULT_BASE_URL, DEFAULT_MAX_RETRIES,
    DEFAULT_TIMEOUT, DEFAULT_WAIT_TIMEOUT, VERSION,
};
pub use conversions::{ConversionRequest, ConvertParams};
pub use download::DownloadOptions;
pub use error::{
    ApiError, ApiErrorKind, ConfirmationRequiredError, ConnectionError, ConversionFailedError,
    Error, Result, WaitTimeoutError,
};
pub use models::{Account, Allowance, Conversion, ErrorInfo, Format, Status, Upload, Wallet};
/// The `reqwest` version this crate is built on, for [`ClientBuilder::http_client`].
pub use reqwest;
