//! Formats and the account.

use reqwest::Method;
use serde::Deserialize;

use crate::client::Client;
use crate::error::{Error, Result};
use crate::models::{Account, Format};
use crate::request::Call;

impl Client {
    /// The formats the API reads and what each converts to right now (`GET /v1/formats`). It
    /// needs no API key.
    pub async fn formats(&self) -> Result<Vec<Format>> {
        self.list_formats(None).await
    }

    /// One format (such as `"heic"`) and what it converts to right now; `None` when it
    /// converts to nothing right now. A format the API does not know fails with a 400
    /// ([`ApiErrorKind::BadRequest`](crate::ApiErrorKind::BadRequest)). It needs no API key.
    pub async fn format(&self, id: &str) -> Result<Option<Format>> {
        let id = id.trim().trim_start_matches('.').to_ascii_lowercase();
        Ok(self
            .list_formats(Some(&id))
            .await?
            .into_iter()
            .find(|f| f.id == id))
    }

    async fn list_formats(&self, from: Option<&str>) -> Result<Vec<Format>> {
        #[derive(Deserialize)]
        struct Formats {
            formats: Vec<Format>,
        }
        let mut call = Call::new(Method::GET, self.api_url(&["v1", "formats"]));
        // With a key, the rate limit is the key's own rather than the address's.
        call.auth = self.inner.api_key.is_some();
        if let Some(from) = from {
            call.url.query_pairs_mut().append_pair("from", from);
        }
        let formats: Formats = self.call_value(&call).await?;
        Ok(formats.formats)
    }

    /// The API key's account: plan, credits left and limits (`GET /v1/account`).
    pub async fn account(&self) -> Result<Account> {
        let call = Call::new(Method::GET, self.api_url(&["v1", "account"])).auth();
        let (data, _) = self.call_json(&call).await?;
        let raw: serde_json::Value =
            serde_json::from_slice(&data).map_err(|e| Error::InvalidResponse(e.to_string()))?;
        let mut account: Account = serde_json::from_value(raw.clone())
            .map_err(|e| Error::InvalidResponse(e.to_string()))?;
        account.raw = raw;
        Ok(account)
    }
}
