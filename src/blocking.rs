//! A blocking client: the same calls without async (the `blocking` feature).
//!
//! It runs the async [`Client`](crate::Client) on a small Tokio runtime of its own, so it must
//! not be used from inside an async runtime (that panics); use the async client there.
//!
//! ```no_run
//! use converter_studio::ConvertParams;
//! use converter_studio::blocking::Client;
//!
//! let client = Client::new()?; // the API key comes from CONVERTER_STUDIO_API_KEY
//! let conversion = client.convert_file("IMG_1234.HEIC", "jpg", ConvertParams::default())?;
//! let path = client.download_file(&conversion, "photos/")?; // photos/IMG_1234.jpg
//! # Ok::<(), converter_studio::Error>(())
//! ```

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use tokio::runtime::Runtime;

use crate::{
    Account, ClientBuilder, Conversion, ConversionRequest, ConvertParams, DownloadOptions, Error,
    Format, Result, Upload,
};

/// A blocking client for the Converter Studio API. Each call blocks the calling thread until
/// it is done. See [`crate::Client`] for what each call does.
pub struct Client {
    inner: crate::Client,
    runtime: Runtime,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("blocking::Client")
            .field(&self.inner)
            .finish()
    }
}

impl Client {
    /// A client with the default settings (see [`crate::Client::new`]).
    pub fn new() -> Result<Client> {
        Client::from_async(crate::Client::new()?)
    }

    /// A blocking client from a [`ClientBuilder`]'s settings:
    /// `blocking::Client::from_builder(Client::builder().api_key(key))`.
    pub fn from_builder(builder: ClientBuilder) -> Result<Client> {
        Client::from_async(builder.build()?)
    }

    /// A blocking client that runs `client` on a runtime of its own.
    pub fn from_async(client: crate::Client) -> Result<Client> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|e| Error::Config(format!("The runtime cannot be set up: {e}")))?;
        Ok(Client {
            inner: client,
            runtime,
        })
    }

    /// The async client underneath.
    pub fn as_async(&self) -> &crate::Client {
        &self.inner
    }

    /// See [`crate::Client::formats`].
    pub fn formats(&self) -> Result<Vec<Format>> {
        self.runtime.block_on(self.inner.formats())
    }

    /// See [`crate::Client::format`].
    pub fn format(&self, id: &str) -> Result<Option<Format>> {
        self.runtime.block_on(self.inner.format(id))
    }

    /// See [`crate::Client::account`].
    pub fn account(&self) -> Result<Account> {
        self.runtime.block_on(self.inner.account())
    }

    /// See [`crate::Client::convert_file`].
    pub fn convert_file(
        &self,
        path: impl AsRef<Path>,
        to: &str,
        params: ConvertParams,
    ) -> Result<Conversion> {
        self.runtime
            .block_on(self.inner.convert_file(path, to, params))
    }

    /// See [`crate::Client::convert_bytes`].
    pub fn convert_bytes(
        &self,
        bytes: impl Into<Bytes>,
        file_name: &str,
        to: &str,
        params: ConvertParams,
    ) -> Result<Conversion> {
        self.runtime
            .block_on(self.inner.convert_bytes(bytes, file_name, to, params))
    }

    /// See [`crate::Client::convert_url`].
    pub fn convert_url(&self, url: &str, to: &str, params: ConvertParams) -> Result<Conversion> {
        self.runtime
            .block_on(self.inner.convert_url(url, to, params))
    }

    /// See [`crate::Client::upload_file`].
    pub fn upload_file(&self, path: impl AsRef<Path>) -> Result<Upload> {
        self.runtime.block_on(self.inner.upload_file(path))
    }

    /// See [`crate::Client::upload_bytes`].
    pub fn upload_bytes(&self, bytes: impl Into<Bytes>, file_name: &str) -> Result<Upload> {
        self.runtime
            .block_on(self.inner.upload_bytes(bytes, file_name))
    }

    /// See [`crate::Client::create_conversion`].
    pub fn create_conversion(&self, request: &ConversionRequest) -> Result<Conversion> {
        self.runtime.block_on(self.inner.create_conversion(request))
    }

    /// See [`crate::Client::conversion`].
    pub fn conversion(&self, id: &str) -> Result<Conversion> {
        self.runtime.block_on(self.inner.conversion(id))
    }

    /// See [`crate::Client::wait_for_update`].
    pub fn wait_for_update(&self, id: &str, max_wait: Duration) -> Result<Conversion> {
        self.runtime
            .block_on(self.inner.wait_for_update(id, max_wait))
    }

    /// See [`crate::Client::wait`].
    pub fn wait(&self, id: &str) -> Result<Conversion> {
        self.runtime.block_on(self.inner.wait(id))
    }

    /// See [`crate::Client::confirm`].
    pub fn confirm(&self, id: &str) -> Result<Conversion> {
        self.runtime.block_on(self.inner.confirm(id))
    }

    /// See [`crate::Client::download`]: the result is written to `writer` as it arrives.
    pub fn download<W: Write + ?Sized>(
        &self,
        conversion: &Conversion,
        writer: &mut W,
    ) -> Result<u64> {
        let mut adapter = SyncWriter(writer);
        self.runtime
            .block_on(self.inner.download(conversion, &mut adapter))
    }

    /// See [`crate::Client::download_bytes`].
    pub fn download_bytes(&self, conversion: &Conversion) -> Result<Vec<u8>> {
        self.runtime.block_on(self.inner.download_bytes(conversion))
    }

    /// See [`crate::Client::download_file`].
    pub fn download_file(
        &self,
        conversion: &Conversion,
        dest: impl AsRef<Path>,
    ) -> Result<PathBuf> {
        self.runtime
            .block_on(self.inner.download_file(conversion, dest))
    }

    /// See [`crate::Client::download_file_with`].
    pub fn download_file_with(
        &self,
        conversion: &Conversion,
        dest: impl AsRef<Path>,
        options: DownloadOptions,
    ) -> Result<PathBuf> {
        self.runtime
            .block_on(self.inner.download_file_with(conversion, dest, options))
    }
}

/// A blocking writer used from the client's own runtime thread (the caller's thread, which is
/// blocked anyway).
struct SyncWriter<'a, W: Write + ?Sized>(&'a mut W);

impl<W: Write + ?Sized> tokio::io::AsyncWrite for SyncWriter<'_, W> {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(self.get_mut().0.write(buf))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(self.get_mut().0.flush())
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
