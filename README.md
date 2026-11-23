# Converter Studio for Rust

[![crates.io](https://img.shields.io/crates/v/converter-studio.svg)](https://crates.io/crates/converter-studio)
[![docs.rs](https://img.shields.io/docsrs/converter-studio)](https://docs.rs/converter-studio)

The official Rust client for the [Converter Studio](https://converter.studio) API. Convert iPhone
photos (HEIC), camera RAW photos, videos, audio, documents, spreadsheets, presentations, e-books,
subtitles, fonts, data files and archives (67 formats) from your Rust code. The conversion runs on
Converter Studio's servers, so there is nothing else to install: no ImageMagick, no FFmpeg, no
LibreOffice, no C libraries to link.

```rust
use converter_studio::{Client, ConvertParams};

let client = Client::new()?; // the API key comes from CONVERTER_STUDIO_API_KEY
let conversion = client
    .convert_file("IMG_1234.HEIC", "jpg", ConvertParams::default())
    .await?;
let path = client.download_file(&conversion, "photos/").await?; // photos/IMG_1234.jpg
```

- Async with Tokio and reqwest (rustls, no OpenSSL); a blocking client behind the `blocking`
  feature; a `converter-studio` command behind the `cli` feature.
- Local files, bytes in memory, or links (direct links and Google Drive, Dropbox or OneDrive share
  links), with any of the API's conversion settings.
- Large files are uploaded in parts on their own; results are streamed to a file or any
  `AsyncWrite`.
- Waiting with the API's long polling and a back-off; retries with an `Idempotency-Key`, so a
  retry never starts or charges a second conversion.
- One error type that carries the API's code and message, a sentence you can show to people.

## Installation

```console
$ cargo add converter-studio
$ cargo add tokio --features macros,rt-multi-thread
```

Rust 1.85 or later. Then create an API key on your
[account page](https://converter.studio/account#api-keys) (keys start with `cvs_live_`; keep them
secret) and put it in the environment:

```console
$ export CONVERTER_STUDIO_API_KEY=cvs_live_...
```

Every conversion uses your account's credits, the same as on the website: free accounts get
credits every day, Pro and Premium their monthly credits. Most files cost 1 credit; audio and video
are priced by their length (video also by its frame size and the format made) once Converter
Studio has looked at them. See [pricing](https://converter.studio/pricing).

## Convert a file

```rust
use converter_studio::{Client, ConvertParams};

#[tokio::main]
async fn main() -> converter_studio::Result<()> {
    let client = Client::new()?; // or Client::builder().api_key("cvs_live_...").build()?
    let params = ConvertParams::new().option("height", "720");
    let conversion = client.convert_file("talk.mov", "mp4", params).await?;
    println!("{:?} {:?}", conversion.output_name, conversion.credits); // Some("talk.mp4") Some(1)
    let path = client.download_file(&conversion, "out/").await?; // a folder, or a file path
    println!("saved {}", path.display());
    Ok(())
}
```

`convert_file` uploads the file, starts the conversion and waits until it is done (10 minutes at
most by default: `ClientBuilder::wait_timeout`). The source can also be:

- bytes in memory, with a name: `client.convert_bytes(data, "report.docx", "pdf", params)`;
- a link: `client.convert_url("https://example.com/photo.heic", "jpg", params)`. Converter Studio
  fetches the file itself, so it never passes through your machine.

`download_file` never replaces an existing file unless asked
(`download_file_with(&conversion, path, DownloadOptions::new().overwrite(true))`): in a folder
the result gets `-1`, `-2`... before its extension. The file appears only once it is complete. You
can also stream the result to any writer (`client.download(&conversion, &mut writer)`) or keep it
in memory (`client.download_bytes(&conversion)`). Results are kept for 6 hours, and anyone with
`conversion.download_url` can download the result until `conversion.expires_at` (the `Debug`
output of a `Conversion` leaves the link out).

## Without async

```toml
[dependencies]
converter-studio = { version = "1", features = ["blocking"] }
```

```rust
use converter_studio::blocking::Client;
use converter_studio::ConvertParams;

let client = Client::new()?;
let conversion = client.convert_file("scan.pdf", "docx", ConvertParams::new().option("ocr", true))?;
client.download_file(&conversion, "out/")?;
```

The blocking client runs the async one on a small runtime of its own; do not call it from inside
an async runtime.

## Settings

`ConvertParams::option(name, value)` takes the API's conversion settings, the same as on the
website. A few of them:

| Setting                                                          | For                                               |
| ---------------------------------------------------------------- | ------------------------------------------------- |
| `quality`: `"high"`, `"balanced"`, `"small"`                     | video, JPG, WebP, AVIF                            |
| `height`: `"1080"`, `"720"`, `"480"`, `"360"`                    | video (never upscaled)                            |
| `maxSize`: MB                                                    | MP4 that must fit a size                          |
| `maxKb`: KB                                                      | JPG, WebP, AVIF that must fit a size              |
| `longSide`, `resizeWidth`, `resizeHeight`, `fit`                 | resizing images                                   |
| `keepMetadata`, `keepLocation`: `true`                           | keep a photo's camera details and GPS (off)       |
| `bitrate`: `"64"`... `"320"`, `channels`, `trimStart`, `trimEnd` | audio                                             |
| `pageRange`: `"1-3, 5"`, `pdfA`: `true`                          | documents to PDF                                  |
| `ocr`: `true`, `ocrLanguage`                                     | scanned PDFs to Word or text                      |
| `compression`: `"light"`, `"balanced"`, `"strong"`               | PDF to a smaller PDF                              |
| `password`                                                       | a locked ZIP, 7Z or RAR (used once, never stored) |

The full list, and which settings apply to which conversion, is on the
[API page](https://converter.studio/developers). Settings that do not apply to a conversion are
ignored; a name the API does not know is refused (`ApiErrorKind::BadRequest`).

## Formats and account

```rust
for format in client.formats().await? { // no key needed
    println!("{} -> {}", format.id, format.targets.join(", "));
}
let heic = client.format("heic").await?; // Some(format) with what HEIC converts to
let account = client.account().await?;
println!("{} {} {:?}", account.plan, account.credits, account.renews_at);
```

## Price limits

A conversion goes ahead on its own when it costs at most its maximum (20 credits by default). A
dearer one waits for your go-ahead and costs nothing until then:

```rust
use converter_studio::{ConvertParams, Error};

let params = ConvertParams::new().max_credits(5);
match client.convert_file("lecture.mkv", "mp4", params).await {
    Err(Error::ConfirmationRequired(quote)) => {
        println!("It costs {:?} credits", quote.credits);
        client.confirm(&quote.conversion.id).await?;
        let done = client.wait(&quote.conversion.id).await?;
    }
    other => { other?; }
}
```

`ConvertParams::new().quote_only()` only gets the price.

## Step by step

`convert_file` is built from these, which you can call yourself:

```rust
use converter_studio::ConversionRequest;
use std::time::Duration;

let upload = client.upload_file("scan.pdf").await?; // large files go in parts
let created = client
    .create_conversion(&ConversionRequest::from_upload(&upload.id, "docx").option("ocr", true))
    .await?;
let created = client
    .create_conversion(&ConversionRequest::from_url("https://example.com/photo.heic", "jpg"))
    .await?;
let now = client.conversion(&created.id).await?; // status now
let later = client.wait_for_update(&created.id, Duration::from_secs(30)).await?; // held up to 30 s
let done = client.wait(&created.id).await?;
```

`wait` returns a finished conversion or an error (see [Errors](#errors)). It lets the API hold
each status request until something changes, and pauses between answers that come back early (1
second, doubling up to 10).

## Callbacks and retries

```rust
let request = ConversionRequest::from_url("https://example.com/raw/clip.mov", "mp4")
    .callback_url("https://example.com/hooks/converter-studio?token=SECRET")
    .idempotency_key("clip-42");
let created = client.create_conversion(&request).await?;
```

- `callback_url`: an `https://` address on the public internet that Converter Studio POSTs to
  when the conversion waits for a go-ahead (`conversion.quoted`) and when it ends
  (`conversion.done`, `conversion.failed` or `conversion.canceled`). The body is
  `{"event", "id", "createdAt", "conversion"}`, with `Webhook-Id` and `Webhook-Timestamp` headers.
  Callbacks are not signed: keep the address secret, and check the conversion with
  `client.conversion(id)` before acting on one.
- `idempotency_key`: sending the same request again with the same key (within 24 hours) returns
  the first conversion (`conversion.replayed` is `true`) instead of starting and charging a second
  one; the same key with a different request fails with `ApiErrorKind::Conflict`. The client makes
  up a key for each `create_conversion` call and reuses it for its own retries, so pass your own
  when your code itself may repeat a request (a job queue retrying a task).

## Errors

Every call returns `converter_studio::Error`, whose text is a sentence meant for people (the API's
own message):

| Variant                                       | When                                                                                                                     |
| --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| `Error::Api`                                  | the API answered with an error: `status`, `code` (`notEnoughCredits`...), `message`, `params`; `kind()` tells them apart |
| `ApiErrorKind::Authentication` (401)          | the API key does not work                                                                                                |
| `ApiErrorKind::InsufficientCredits` (402)     | not enough credits (`params`: `needed`, `balance`)                                                                       |
| `ApiErrorKind::FileTooLarge`, `Unprocessable` | the file is over the plan's limit (413); the file cannot be converted that way (422, `unsupportedConversion`)            |
| `ApiErrorKind::RateLimited`, `Server`         | 429 and 5xx, retried on their own first (`max_retries`, 2 by default); `retry_after`                                     |
| `Error::ConversionFailed`                     | the conversion failed, expired or was canceled: `code`, `message`, `conversion`; failures are not charged                |
| `Error::ConfirmationRequired`                 | the price is over the maximum: `credits`, `conversion`                                                                   |
| `Error::WaitTimeout`                          | not done within the wait timeout; it may still finish (`conversion.id`)                                                  |
| `Error::MissingApiKey`, `Error::Connection`   | no key; the API could not be reached (after the retries; `timeout`)                                                      |

```rust
use converter_studio::{ApiErrorKind, ConvertParams, Error};

match client.convert_file("broken.heic", "jpg", ConvertParams::default()).await {
    Ok(conversion) => println!("done: {:?}", conversion.output_name),
    Err(Error::ConversionFailed(failed)) => println!("{}: {}", failed.code, failed.message),
    Err(e) if e.api_kind() == Some(ApiErrorKind::InsufficientCredits) => {
        println!("Buy credits at https://converter.studio/pricing");
    }
    Err(e) => println!("{e}"),
}
```

## Configuration

```rust
use std::time::Duration;

let client = converter_studio::Client::builder()
    .api_key("cvs_live_...")                     // default: CONVERTER_STUDIO_API_KEY
    .base_url("https://converter.studio/api")    // default: CONVERTER_STUDIO_BASE_URL or this
    .timeout(Duration::from_secs(60))            // a request without progress
    .max_retries(2)                              // network errors, 408, 429, 5xx
    .wait_timeout(Duration::from_secs(600))      // how long wait() waits (None: no limit)
    .user_agent("my-app/1.2")                    // put in front of the User-Agent
    .http_client(reqwest_client)                 // your own reqwest::Client (proxies, certificates)
    .build()?;
```

The timeout is not a limit on a whole call (a large file takes as long as it takes): it ends a
request that goes without progress. The API key is only sent to the base URL; upload and download
links carry their own tokens. A `Client` is cheap to clone and safe to share between tasks.
`converter_studio::reqwest` is the reqwest version the crate is built on, for `http_client`.

## Command line

```console
$ cargo install converter-studio --features cli
$ converter-studio convert IMG_1234.HEIC IMG_1235.HEIC --to jpg -o photos
$ converter-studio convert talk.mov --to mp4 -O height=720
$ converter-studio formats heic
$ converter-studio account
```

`converter-studio --help` lists the options. Exit codes: 0 all done, 1 a conversion failed, 2 wrong
usage, 3 no API key or a key that does not work, 4 not enough credits or a price over
`--max-credits`, 5 a conversion that did not finish within `--timeout`.

## Limits

- The largest file depends on the plan (`account.max_file_bytes`). One request carries up to
  95 MB; the client sends larger files in parts on its own.
- Audio and video length also depends on the plan (`account.max_media_seconds`).
- Results are kept for 6 hours, then deleted. A status request can wait up to 60 seconds.
- One file in, one file out: no merging or splitting of PDFs.

## Privacy

Your files are sent to Converter Studio (converter.studio) to be converted. Uploaded files are
deleted as soon as they are converted, and results 6 hours later. Each request also says which
client sent it (`X-Converter-Studio-Client: rust/<version>`), for usage statistics only. See the
[privacy policy](https://converter.studio/privacy) and the [terms](https://converter.studio/terms).

## Links

- [Converter Studio](https://converter.studio)
- [API documentation](https://converter.studio/developers)
- [Pricing](https://converter.studio/pricing)
- [Crate documentation](https://docs.rs/converter-studio)
- [Source code and issues](https://github.com/tariksucu/converter-studio-rust)
- [Changelog](https://github.com/tariksucu/converter-studio-rust/blob/main/CHANGELOG.md)

Support: <support@converter.studio>

MIT licensed. © Converter Studio.
