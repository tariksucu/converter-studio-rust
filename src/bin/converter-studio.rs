//! `converter-studio`: convert files from the command line (the `cli` feature).
//!
//! ```console
//! $ cargo install converter-studio --features cli
//! $ export CONVERTER_STUDIO_API_KEY=cvs_live_...
//! $ converter-studio convert IMG_1234.HEIC --to jpg
//! ```
//!
//! Exit codes: 0 all done; 1 a conversion or request failed; 2 wrong usage; 3 no API key, or
//! the key does not work; 4 not enough credits, or the price is over `--max-credits`; 5 a
//! conversion did not finish within `--timeout`. With several files, the first failure's.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use converter_studio::blocking::Client;
use converter_studio::{
    Account, ApiErrorKind, ClientBuilder, ConvertParams, DownloadOptions, Error, VERSION,
};
use serde_json::Value;

const HELP: &str = "Usage: converter-studio <command> [options]

Convert files with Converter Studio (https://converter.studio).

Commands:
  convert FILE_OR_URL... --to FORMAT   convert files or links and save the results
  formats [--from FORMAT]              list the formats and what they convert to (no key needed)
  account                              show the key's plan and credits

Options of convert:
  -t, --to FORMAT          the target format (jpg, mp4, pdf...)
  -o, --output-dir DIR     where results go (default: the current folder)
  -O, --option KEY=VALUE   a conversion setting, repeated for more; KEY:=JSON sends a JSON value
  --max-credits N          the most a conversion may cost to start on its own (default 20)
  --timeout SECONDS        how long to wait for each conversion (default 600)
  --overwrite              replace existing files (otherwise the result gets -1, -2...)
  -q, --quiet              print only errors

Options of every command:
  --api-key-file PATH      read the API key from a file instead of $CONVERTER_STUDIO_API_KEY
  --base-url URL           another API address (default: $CONVERTER_STUDIO_BASE_URL or
                           https://converter.studio/api)
  -h, --help, -V, --version

The API key is read from $CONVERTER_STUDIO_API_KEY; create one at
https://converter.studio/account#api-keys. Documentation: https://converter.studio/developers
";

const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_KEY: u8 = 3;
const EXIT_CREDITS: u8 = 4;
const EXIT_TIMEOUT: u8 = 5;

/// The command line, parsed.
#[derive(Debug, Default, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
struct Args {
    command: String,
    inputs: Vec<String>,
    to: Option<String>,
    output_dir: Option<PathBuf>,
    options: Vec<(String, Value)>,
    max_credits: Option<u32>,
    timeout: Option<Duration>,
    overwrite: bool,
    quiet: bool,
    from: Option<String>,
    api_key_file: Option<PathBuf>,
    base_url: Option<String>,
    help: bool,
    version: bool,
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("converter-studio: {message}\n\n{HELP}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    ExitCode::from(run(&args))
}

fn run(args: &Args) -> u8 {
    if args.version {
        println!("converter-studio {VERSION}");
        return 0;
    }
    if args.help || args.command.is_empty() {
        if args.help {
            print!("{HELP}");
            return 0;
        }
        eprint!("{HELP}");
        return EXIT_USAGE;
    }
    if !matches!(args.command.as_str(), "convert" | "formats" | "account") {
        eprintln!(
            "converter-studio: unknown command: {} (convert, formats or account)",
            args.command
        );
        return EXIT_USAGE;
    }
    let client = match client(args) {
        Ok(client) => client,
        Err((message, code)) => {
            eprintln!("converter-studio: {message}");
            return code;
        }
    };
    let result = match args.command.as_str() {
        "convert" => return convert(&client, args),
        "formats" => formats(&client, args.from.as_deref()),
        _ => client.account().map(|account| print_account(&account)),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("converter-studio: {}", describe(&e));
            exit_code(&e)
        }
    }
}

fn client(args: &Args) -> Result<Client, (String, u8)> {
    let mut builder = ClientBuilder::new();
    if let Some(file) = &args.api_key_file {
        let key = std::fs::read_to_string(file)
            .map_err(|e| (format!("cannot read {}: {e}", file.display()), EXIT_KEY))?;
        if key.trim().is_empty() {
            return Err((
                format!("The API key file {} is empty.", file.display()),
                EXIT_KEY,
            ));
        }
        builder = builder.api_key(key);
    }
    if let Some(url) = &args.base_url {
        builder = builder.base_url(url);
    }
    builder = builder
        .user_agent(format!("converter-studio-cli/{VERSION}"))
        .wait_timeout(args.timeout.unwrap_or(Duration::from_secs(600)));
    Client::from_builder(builder).map_err(|e| (e.to_string(), EXIT_USAGE))
}

fn convert(client: &Client, args: &Args) -> u8 {
    let Some(to) = args.to.as_deref() else {
        eprintln!("converter-studio: convert needs --to FORMAT");
        return EXIT_USAGE;
    };
    if args.inputs.is_empty() {
        eprintln!("converter-studio: convert needs at least one file or link");
        return EXIT_USAGE;
    }
    let missing: Vec<&str> = args
        .inputs
        .iter()
        .map(String::as_str)
        .filter(|item| !is_link(item) && !Path::new(item).is_file())
        .collect();
    if !missing.is_empty() {
        eprintln!("converter-studio: no such file: {}", missing.join(", "));
        return EXIT_USAGE;
    }
    let dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("converter-studio: cannot create {}: {e}", dir.display());
        return EXIT_FAILED;
    }
    let mut first_failure = 0;
    for (index, item) in args.inputs.iter().enumerate() {
        let label = if is_link(item) {
            item.clone()
        } else {
            Path::new(item)
                .file_name()
                .map_or_else(|| item.clone(), |n| n.to_string_lossy().into_owned())
        };
        if !args.quiet {
            eprintln!("[{}/{}] {label} -> {to}", index + 1, args.inputs.len());
        }
        let mut params = ConvertParams::new();
        for (key, value) in &args.options {
            params = params.option(key.clone(), value.clone());
        }
        if let Some(credits) = args.max_credits {
            params = params.max_credits(credits);
        }
        let result = if is_link(item) {
            client.convert_url(item, to, params)
        } else {
            client.convert_file(item, to, params)
        }
        .and_then(|conversion| {
            let options = DownloadOptions::new().overwrite(args.overwrite);
            let path = client.download_file_with(&conversion, &dir, options)?;
            Ok((conversion, path))
        });
        match result {
            Ok((conversion, path)) => {
                if !args.quiet {
                    let credits = conversion.credits.map_or("?".to_owned(), |c| c.to_string());
                    eprintln!(
                        "  saved {} ({}, {credits} credit{})",
                        path.display(),
                        human_size(conversion.output_size),
                        if conversion.credits == Some(1) {
                            ""
                        } else {
                            "s"
                        }
                    );
                }
                println!("{}", path.display());
            }
            Err(e) => {
                let code = exit_code(&e);
                if first_failure == 0 {
                    first_failure = code;
                }
                eprintln!("converter-studio: {label}: {}", describe(&e));
                // A key that does not work, or no credits: the next files would fail the same way.
                if code == EXIT_KEY || e.api_kind() == Some(ApiErrorKind::InsufficientCredits) {
                    break;
                }
            }
        }
    }
    first_failure
}

fn formats(client: &Client, from: Option<&str>) -> converter_studio::Result<()> {
    let formats = match from {
        Some(from) => match client.format(from) {
            Ok(found) => found.into_iter().collect(),
            Err(e) if e.api_kind() == Some(ApiErrorKind::BadRequest) => Vec::new(),
            Err(e) => return Err(e),
        },
        None => client.formats()?,
    };
    if formats.is_empty() {
        return Err(Error::InvalidArgument(match from {
            Some(from) => format!("no such format: {from}"),
            None => "no formats".to_owned(),
        }));
    }
    let width = formats.iter().map(|f| f.id.len()).max().unwrap_or(0);
    let name_width = formats.iter().map(|f| f.name.len()).max().unwrap_or(0);
    for format in &formats {
        let targets = if format.targets.is_empty() {
            "-".to_owned()
        } else {
            format.targets.join(", ")
        };
        println!(
            "{:width$}  {:name_width$}  {:12}  -> {targets}",
            format.id, format.name, format.category
        );
    }
    Ok(())
}

fn print_account(account: &Account) {
    println!("{:10} {}", "Account:", account.email);
    let all_apps = if account.all_apps { " (All Apps)" } else { "" };
    println!("{:10} {}{all_apps}", "Plan:", account.plan);
    let mut credits = format!("{} left", account.credits);
    if let Some(allowance) = &account.allowance {
        let per = if allowance.per.is_empty() {
            "period"
        } else {
            &allowance.per
        };
        let packs = if account.packs > 0 {
            format!(", {} in packs", account.packs)
        } else {
            String::new()
        };
        let used = format!(
            " ({} of {} used this {per}{packs})",
            allowance.used, allowance.limit
        );
        credits.push_str(&used);
    }
    if let Some(renews) = &account.renews_at {
        credits.push_str(", renews ");
        credits.push_str(renews.get(..10).unwrap_or(renews));
    }
    println!("{:10} {credits}", "Credits:");
    let media = if account.max_media_seconds > 0 {
        format!(
            ", audio and video up to {} min",
            account.max_media_seconds / 60
        )
    } else {
        String::new()
    };
    println!(
        "{:10} files up to {}{media}",
        "Limits:",
        human_size(Some(account.max_file_bytes))
    );
    if let Some(wallet) = &account.wallet {
        let pays = if wallet.pays {
            ", pays past the credits"
        } else {
            ""
        };
        #[allow(clippy::cast_precision_loss)]
        let dollars = wallet.balance_micros as f64 / 1_000_000.0;
        println!("{:10} ${dollars:.2}{pays}", "Wallet:");
    }
}

fn exit_code(error: &Error) -> u8 {
    match error {
        Error::MissingApiKey => EXIT_KEY,
        Error::Api(e) if e.kind() == ApiErrorKind::Authentication => EXIT_KEY,
        Error::Api(e) if e.kind() == ApiErrorKind::InsufficientCredits => EXIT_CREDITS,
        Error::ConfirmationRequired(_) => EXIT_CREDITS,
        Error::WaitTimeout(_) => EXIT_TIMEOUT,
        _ => EXIT_FAILED,
    }
}

/// An error in one line: the API's message, and its code when there is one.
fn describe(error: &Error) -> String {
    match error {
        Error::ConfirmationRequired(quote) => format!(
            "{quote} Run again with --max-credits {} to go ahead.",
            quote.credits.map_or("?".to_owned(), |c| c.to_string())
        ),
        Error::Api(e) if e.code.as_deref() == Some("invalidRequest") => format!(
            "{} [invalidRequest] Check the target format (converter-studio formats) and the \
             options.",
            e.message
        ),
        _ => match error.code() {
            Some(code) => format!("{error} [{code}]"),
            None => error.to_string(),
        },
    }
}

fn is_link(item: &str) -> bool {
    let lower = item.to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

fn human_size(size: Option<u64>) -> String {
    let Some(size) = size else {
        return "?".to_owned();
    };
    if size < 1000 {
        return format!("{size} B");
    }
    #[allow(clippy::cast_precision_loss)]
    let mut value = size as f64 / 1000.0;
    for unit in ["KB", "MB"] {
        if value < 1000.0 {
            return format!("{value:.1} {unit}");
        }
        value /= 1000.0;
    }
    format!("{value:.1} GB")
}

/// The settings whose values are not text, by type. Every other setting is text, also those
/// that look like numbers (`height=720` is the text `"720"`).
fn option_type(key: &str) -> Option<&'static str> {
    Some(match key {
        "mute" | "copyVideo" | "normalize" | "fade" | "copyAudio" | "keepMetadata"
        | "keepLocation" | "lossless" | "flip" | "pdfA" | "sheetsOnePage" | "ocr" | "header"
        | "tocPage" | "hiddenSlides" => "boolean",
        "maxSize" | "resizeWidth" | "resizeHeight" | "maxKb" | "sheet" => "integer",
        "trimStart" | "trimEnd" | "start" | "subtitleShift" => "number",
        _ => return None,
    })
}

/// One `-O` setting: `KEY=VALUE` (typed by the setting) or `KEY:=JSON`.
fn parse_option(pair: &str) -> Result<(String, Value), String> {
    let Some((key, value)) = pair.split_once('=').filter(|(k, _)| !k.trim().is_empty()) else {
        return Err(format!("--option takes KEY=VALUE, not {pair:?}"));
    };
    if let Some(key) = key.strip_suffix(':') {
        let key = key.trim();
        return serde_json::from_str(value)
            .map(|v| (key.to_owned(), v))
            .map_err(|_| format!("{key}:= takes a JSON value, not {value:?}"));
    }
    let key = key.trim();
    let text = value.trim();
    let typed = match option_type(key) {
        Some("boolean") => match text.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Value::Bool(true),
            "false" | "no" | "off" | "0" => Value::Bool(false),
            _ => return Err(format!("{key} takes true or false, not {value:?}")),
        },
        Some("integer") => text
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| format!("{key} takes a whole number, not {value:?}"))?,
        Some(_) => text
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .map(Value::from)
            .ok_or_else(|| format!("{key} takes a number, not {value:?}"))?,
        None => Value::String(value.to_owned()),
    };
    Ok((key.to_owned(), typed))
}

/// Parses the arguments after the program's name.
fn parse(words: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut args = Args::default();
    let mut positionals = Vec::new();
    let mut words = words.into_iter();
    let mut only_positionals = false;
    while let Some(arg) = words.next() {
        if only_positionals || arg == "-" || !arg.starts_with('-') {
            positionals.push(arg);
            continue;
        }
        if arg == "--" {
            only_positionals = true;
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_owned(), Some(value.to_owned()))
            }
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| words.next())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "-t" | "--to" => args.to = Some(value("--to")?),
            "-o" | "--output-dir" => args.output_dir = Some(value("--output-dir")?.into()),
            "-O" | "--option" => args.options.push(parse_option(&value("--option")?)?),
            "--max-credits" => {
                let text = value("--max-credits")?;
                args.max_credits =
                    Some(text.trim().parse().map_err(|_| {
                        "--max-credits must be a whole number, 0 or more".to_owned()
                    })?);
            }
            "--timeout" => {
                let seconds: f64 = value("--timeout")?.trim().parse().unwrap_or(0.0);
                if !(seconds.is_finite() && seconds > 0.0) {
                    return Err("--timeout must be a number of seconds above 0".to_owned());
                }
                args.timeout = Some(Duration::from_secs_f64(seconds.min(86_400.0)));
            }
            "--from" => args.from = Some(value("--from")?),
            "--api-key-file" => args.api_key_file = Some(value("--api-key-file")?.into()),
            "--base-url" => args.base_url = Some(value("--base-url")?),
            "--overwrite" => args.overwrite = true,
            "-q" | "--quiet" => args.quiet = true,
            "-h" | "--help" => args.help = true,
            "-V" | "--version" => args.version = true,
            _ => return Err(format!("unknown option: {flag}")),
        }
    }
    let mut positionals = positionals.into_iter();
    args.command = positionals.next().unwrap_or_default();
    args.inputs = positionals.collect();
    if args.command == "formats" && args.from.is_none() && args.inputs.len() == 1 {
        args.from = args.inputs.pop();
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse_str(line: &str) -> Result<Args, String> {
        parse(line.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn parses_a_conversion() {
        let args = parse_str(
            "convert a.heic https://example.com/b.png -t jpg -o out -O quality=high -O ocr=yes \
             -O maxKb=500 -O trimStart=1.5 -O pages:=[1,2] --max-credits 5 --overwrite -q",
        )
        .unwrap();
        assert_eq!(args.command, "convert");
        assert_eq!(args.inputs, ["a.heic", "https://example.com/b.png"]);
        assert_eq!(args.to.as_deref(), Some("jpg"));
        assert_eq!(args.output_dir, Some(PathBuf::from("out")));
        assert_eq!(
            args.options,
            [
                ("quality".to_owned(), json!("high")),
                ("ocr".to_owned(), json!(true)),
                ("maxKb".to_owned(), json!(500)),
                ("trimStart".to_owned(), json!(1.5)),
                ("pages".to_owned(), json!([1, 2])),
            ]
        );
        assert_eq!(args.max_credits, Some(5));
        assert!(args.overwrite && args.quiet);
    }

    #[test]
    fn takes_values_after_an_equals_sign() {
        let args =
            parse_str("convert --to=pdf --timeout=30 --base-url=http://localhost:1/api x.docx")
                .unwrap();
        assert_eq!(args.to.as_deref(), Some("pdf"));
        assert_eq!(args.timeout, Some(Duration::from_secs(30)));
        assert_eq!(args.base_url.as_deref(), Some("http://localhost:1/api"));
        assert_eq!(args.inputs, ["x.docx"]);
    }

    #[test]
    fn formats_takes_a_format() {
        assert_eq!(
            parse_str("formats heic").unwrap().from.as_deref(),
            Some("heic")
        );
        assert_eq!(
            parse_str("formats --from mp4").unwrap().from.as_deref(),
            Some("mp4")
        );
    }

    #[test]
    fn refuses_wrong_usage() {
        for line in [
            "convert a.heic --to",
            "convert a.heic --nope",
            "convert a.heic -O ocr=maybe",
            "convert a.heic -O maxKb=big",
            "convert a.heic -O =x",
            "convert a.heic -O pages:=[1,",
            "convert a.heic --max-credits -1",
            "convert a.heic --timeout 0",
        ] {
            assert!(parse_str(line).is_err(), "{line}");
        }
    }

    #[test]
    fn keeps_text_settings_as_text() {
        assert_eq!(
            parse_option("height=720").unwrap(),
            ("height".to_owned(), json!("720"))
        );
        assert_eq!(
            parse_option("pageRange=1-3, 5").unwrap(),
            ("pageRange".to_owned(), json!("1-3, 5"))
        );
    }

    #[test]
    fn sizes_for_people() {
        assert_eq!(human_size(None), "?");
        assert_eq!(human_size(Some(999)), "999 B");
        assert_eq!(human_size(Some(1500)), "1.5 KB");
        assert_eq!(human_size(Some(2_500_000)), "2.5 MB");
        assert_eq!(human_size(Some(3_000_000_000)), "3.0 GB");
    }

    #[test]
    fn exit_codes() {
        assert_eq!(exit_code(&Error::MissingApiKey), EXIT_KEY);
        assert_eq!(
            exit_code(&Error::InvalidArgument(String::new())),
            EXIT_FAILED
        );
    }
}
