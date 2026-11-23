//! Downloading results: to a writer, into memory or to a file.

use std::path::{Path, PathBuf};

use reqwest::{Method, Url};
use tokio::io::{AsyncSeekExt, AsyncWrite, AsyncWriteExt};

use crate::client::Client;
use crate::conversions::settled;
use crate::error::{Error, Result};
use crate::models::Conversion;
use crate::request::{Call, random_u64};

/// Settings of [`Client::download_file_with`].
#[derive(Clone, Debug, Default)]
#[must_use]
pub struct DownloadOptions {
    overwrite: bool,
}

impl DownloadOptions {
    /// The default: never replace an existing file.
    pub fn new() -> Self {
        DownloadOptions::default()
    }

    /// Replaces an existing file. Without it, a result saved into a folder gets `-1`, `-2`...
    /// before its extension, and an exact path that exists fails.
    pub fn overwrite(mut self, overwrite: bool) -> Self {
        self.overwrite = overwrite;
        self
    }
}

impl Client {
    /// Writes the result of a done conversion to `writer` and returns the number of bytes
    /// written. The result is streamed: nothing is kept in memory. It is sent again after a
    /// network error only while nothing has been written yet.
    ///
    /// A conversion that is not done fails with [`Error::NotDone`], one that failed with
    /// [`Error::ConversionFailed`]. Results are kept for 6 hours
    /// ([`Conversion::expires_at`]); a deleted one fails with a 410 (`fileExpired`).
    pub async fn download<W>(&self, conversion: &Conversion, writer: &mut W) -> Result<u64>
    where
        W: AsyncWrite + Unpin + ?Sized,
    {
        let call = download_call(conversion)?;
        let mut attempt = 0;
        loop {
            let mut written = 0;
            let result = async {
                let (mut response, progress) = self.send(&call).await?;
                while let Some(chunk) = self.next_chunk(&mut response, &progress, &call.url).await?
                {
                    writer.write_all(&chunk).await?;
                    written += chunk.len() as u64;
                }
                writer.flush().await?;
                Ok(())
            }
            .await;
            match result {
                Ok(()) => return Ok(written),
                Err(e) if written > 0 => return Err(e),
                Err(e) => self.before_retry(e, &mut attempt).await?,
            }
        }
    }

    /// The result of a done conversion, in memory. For large results prefer
    /// [`Client::download_file`] or [`Client::download`].
    pub async fn download_bytes(&self, conversion: &Conversion) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        self.download(conversion, &mut data).await?;
        Ok(data)
    }

    /// Saves the result of a done conversion and returns the file's path.
    ///
    /// `dest` is a folder (one that exists, or a path ending in a slash), into which the
    /// result goes under its own name; or the path of the file to write; or `""` for the
    /// current folder. An existing file is never replaced (see
    /// [`download_file_with`](Self::download_file_with)): in a folder the result gets `-1`,
    /// `-2`... before its extension. The file appears only once it is complete (it is written
    /// under a temporary name first), and a broken download is started again.
    pub async fn download_file(
        &self,
        conversion: &Conversion,
        dest: impl AsRef<Path>,
    ) -> Result<PathBuf> {
        self.download_file_with(conversion, dest, DownloadOptions::default())
            .await
    }

    /// [`Client::download_file`] with options, such as replacing an existing file.
    pub async fn download_file_with(
        &self,
        conversion: &Conversion,
        dest: impl AsRef<Path>,
        options: DownloadOptions,
    ) -> Result<PathBuf> {
        let call = download_call(conversion)?;
        let fallback = format!(
            "{}.{}",
            conversion.id,
            conversion
                .to
                .as_deref()
                .filter(|t| !t.is_empty())
                .unwrap_or("bin")
        );
        let name = safe_file_name(conversion.output_name.as_deref().unwrap_or(""), &fallback);
        let (mut path, exact) = target_path(dest.as_ref(), &name).await;
        if !options.overwrite {
            if exact && tokio::fs::symlink_metadata(&path).await.is_ok() {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("{} already exists", path.display()),
                )));
            }
            path = free_path(&path).await?;
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        let (mut file, temporary) = create_temporary(&path).await?;
        let result = async {
            let mut attempt = 0;
            loop {
                let outcome = async {
                    file.rewind().await?;
                    file.set_len(0).await?;
                    let (mut response, progress) = self.send(&call).await?;
                    while let Some(chunk) =
                        self.next_chunk(&mut response, &progress, &call.url).await?
                    {
                        file.write_all(&chunk).await?;
                    }
                    file.flush().await?;
                    Ok(())
                }
                .await;
                match outcome {
                    Ok(()) => break,
                    Err(e @ Error::Io(_)) => return Err(e),
                    Err(e) => self.before_retry(e, &mut attempt).await?,
                }
            }
            file.sync_all().await?;
            drop(file);
            tokio::fs::rename(&temporary, &path).await?;
            Ok(())
        }
        .await;
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(e);
        }
        Ok(path)
    }
}

fn download_call(conversion: &Conversion) -> Result<Call> {
    settled(conversion.clone())?;
    let link = conversion
        .download_url
        .as_deref()
        .filter(|l| !l.is_empty())
        .ok_or_else(|| Error::InvalidResponse("the conversion has no download link".to_owned()))?;
    let url = Url::parse(link)
        .map_err(|_| Error::InvalidResponse("the download link is not a URL".to_owned()))?;
    Ok(Call::new(Method::GET, url))
}

/// A name to save a result under: its last part only, without control characters or slashes,
/// or `fallback`.
pub(crate) fn safe_file_name(name: &str, fallback: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Where a result goes, and whether `dest` named the file itself.
async fn target_path(dest: &Path, name: &str) -> (PathBuf, bool) {
    let text = dest.as_os_str().to_string_lossy();
    if text.is_empty() {
        return (PathBuf::from(name), false);
    }
    if text.ends_with('/') || text.ends_with(std::path::MAIN_SEPARATOR) {
        return (dest.join(name), false);
    }
    match tokio::fs::metadata(dest).await {
        Ok(meta) if meta.is_dir() => (dest.join(name), false),
        _ => (dest.to_path_buf(), true),
    }
}

/// `path`, or with `-1`, `-2`... before its extension when a file is already there.
async fn free_path(path: &Path) -> Result<PathBuf> {
    if tokio::fs::symlink_metadata(path).await.is_err() {
        return Ok(path.to_path_buf());
    }
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, ext) = split_extension(&base);
    for n in 1..10_000 {
        let candidate = path.with_file_name(format!("{stem}-{n}{ext}"));
        if tokio::fs::symlink_metadata(&candidate).await.is_err() {
            return Ok(candidate);
        }
    }
    Err(Error::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("no free name next to {}", path.display()),
    )))
}

/// A file name's stem and extension (with its dot; `.tar.gz` counts as one).
pub(crate) fn split_extension(name: &str) -> (&str, &str) {
    if let Some(stem) = name.strip_suffix(".tar.gz") {
        if !stem.is_empty() {
            return (stem, &name[stem.len()..]);
        }
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    }
}

/// A new hidden file next to `path`.
async fn create_temporary(path: &Path) -> Result<(tokio::fs::File, PathBuf)> {
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for _ in 0..100 {
        let name = format!(".{base}.{:08x}.part", random_u64() & 0xffff_ffff);
        let temporary = path.with_file_name(name);
        match tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
        {
            Ok(file) => return Ok((file, temporary)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("no free temporary name next to {}", path.display()),
    )))
}
