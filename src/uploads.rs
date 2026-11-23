//! Uploading files: one request, or parts for large files.

use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use reqwest::{Method, Url};
use serde_json::{Map, Value, json};

use crate::client::{Client, DEFAULT_PART_BYTES};
use crate::error::{ApiErrorKind, Error, Result};
use crate::models::Upload;
use crate::request::{Call, CallBody, Source};

impl Client {
    /// Opens an upload slot for a file (`POST /v1/uploads`). [`Client::upload_file`] and
    /// [`Client::upload_bytes`] do this and send the file.
    pub async fn create_upload(&self, file_name: &str, size: Option<u64>) -> Result<Upload> {
        if file_name.is_empty() {
            return Err(Error::InvalidArgument(
                "An upload needs a file name (such as \"photo.heic\").".to_owned(),
            ));
        }
        let mut body = Map::new();
        body.insert("fileName".to_owned(), Value::String(file_name.to_owned()));
        if let Some(size) = size {
            body.insert("size".to_owned(), json!(size));
        }
        let call = Call::new(Method::POST, self.api_url(&["v1", "uploads"]))
            .auth()
            .json(&Value::Object(body));
        let upload: Upload = self.call_value(&call).await?;
        if upload.id.is_empty() || upload.upload_url.is_empty() {
            return Err(Error::InvalidResponse(
                "the upload slot has no id or link".to_owned(),
            ));
        }
        Ok(upload)
    }

    /// Sends a local file and returns its upload; convert it with
    /// [`Client::create_conversion`] and
    /// [`ConversionRequest::from_upload`](crate::ConversionRequest::from_upload). Files larger
    /// than one request carries (95 MB) go in parts, and a part that fails is sent again.
    pub async fn upload_file(&self, path: impl AsRef<Path>) -> Result<Upload> {
        let path = path.as_ref();
        let metadata = tokio::fs::metadata(path).await?;
        if !metadata.is_file() {
            return Err(Error::InvalidArgument(format!(
                "{} is not a file.",
                path.display()
            )));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let source = Arc::new(Source::File(path.to_path_buf()));
        self.upload(source, &name, metadata.len()).await
    }

    /// Sends bytes in memory as a file named `file_name` (the API reads the format from the
    /// bytes and the name) and returns its upload.
    pub async fn upload_bytes(&self, bytes: impl Into<Bytes>, file_name: &str) -> Result<Upload> {
        let bytes = bytes.into();
        let size = bytes.len() as u64;
        self.upload(Arc::new(Source::Bytes(bytes)), file_name, size)
            .await
    }

    async fn upload(&self, source: Arc<Source>, name: &str, size: u64) -> Result<Upload> {
        let slot = self.create_upload(name, Some(size)).await?;
        let url = Url::parse(&slot.upload_url)
            .map_err(|_| Error::InvalidResponse("the upload link is not a URL".to_owned()))?;
        let mut part_bytes = if slot.part_bytes > 0 {
            slot.part_bytes
        } else {
            DEFAULT_PART_BYTES
        };
        if let Some(own) = self.inner.part_bytes {
            part_bytes = part_bytes.min(own);
        }
        let ranges = part_ranges(size, part_bytes);
        let in_parts = ranges.len() > 1;
        for (offset, length) in ranges {
            let mut call = Call::new(Method::PUT, url.clone());
            call.body = CallBody::Part {
                source: Arc::clone(&source),
                offset,
                length,
            };
            if in_parts {
                call = call.header("Upload-Offset", offset.to_string());
            }
            self.send_part(&call).await?;
        }
        Ok(slot)
    }

    /// Sends one part, again after network errors and 408, 429 and 5xx answers. A 404 after
    /// the first attempt means an earlier attempt got there (a slot is gone once its last part
    /// is in).
    async fn send_part(&self, call: &Call) -> Result<()> {
        let mut attempt = 0;
        loop {
            let result = async {
                let (response, progress) = self.send(call).await?;
                // The answer is read, so that the connection is kept for the next part.
                self.read_body(response, &progress, 1 << 20, &call.url)
                    .await
                    .map(drop)
            }
            .await;
            match result {
                Ok(()) => return Ok(()),
                Err(e) if attempt > 0 && e.api_kind() == Some(ApiErrorKind::NotFound) => {
                    return Ok(());
                }
                Err(e) => self.before_retry(e, &mut attempt).await?,
            }
        }
    }
}

/// The offset and length of each part; one empty part for an empty file.
pub(crate) fn part_ranges(size: u64, part_bytes: u64) -> Vec<(u64, u64)> {
    if size <= part_bytes {
        return vec![(0, size)];
    }
    (0..size)
        .step_by(usize::try_from(part_bytes).unwrap_or(usize::MAX))
        .map(|offset| (offset, part_bytes.min(size - offset)))
        .collect()
}
