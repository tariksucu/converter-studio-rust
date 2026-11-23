//! Converts a file and saves the result next to it.
//!
//! ```console
//! $ export CONVERTER_STUDIO_API_KEY=cvs_live_...
//! $ cargo run --example convert -- IMG_1234.HEIC jpg
//! ```
//!
//! Create an API key at <https://converter.studio/account#api-keys>.

use std::path::Path;
use std::process::ExitCode;

use converter_studio::{Client, ConvertParams, Error};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [file, to] = args.as_slice() else {
        eprintln!("usage: convert FILE FORMAT   (such as: convert IMG_1234.HEIC jpg)");
        return ExitCode::from(2);
    };
    match run(file, to).await {
        Ok(path) => {
            println!("saved {}", path.display());
            ExitCode::SUCCESS
        }
        Err(Error::ConfirmationRequired(quote)) => {
            eprintln!("{quote}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(file: &str, to: &str) -> converter_studio::Result<std::path::PathBuf> {
    let client = Client::new()?;
    let conversion = client
        .convert_file(file, to, ConvertParams::default())
        .await?;
    let folder = Path::new(file)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    client.download_file(&conversion, folder).await
}
