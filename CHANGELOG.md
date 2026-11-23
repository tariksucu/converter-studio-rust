# Changelog

All notable changes to this crate are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate uses
[semantic versioning](https://semver.org/) (tags `vX.Y.Z`).

## 1.0.0 - 2026-11-23

The first release.

- `Client` with a builder (API key from `api_key` or `CONVERTER_STUDIO_API_KEY`, base URL, your
  own `reqwest::Client`, user agent, timeout, retries, wait timeout, part size). Async with Tokio
  and reqwest (rustls); Rust 1.85 or later.
- Convert a local file (`convert_file`), bytes in memory (`convert_bytes`) or a link
  (`convert_url`) with any of the API's settings; wait with long polling and a back-off; save the
  result to a file (`download_file`), stream it to a writer (`download`) or keep it in memory
  (`download_bytes`); list formats; read the account's plan and credits.
- Uploads in parts for files over 95 MB, retries for network errors, 408, 429 and 5xx answers,
  an `Idempotency-Key` on every conversion, `callback_url`, `max_credits`, `quote_only` and
  `confirm`, and one `Error` type with the API's code and message.
- The `blocking` feature: `converter_studio::blocking::Client`, the same calls without async.
- The `cli` feature: the `converter-studio` command (`convert`, `formats`, `account`).
