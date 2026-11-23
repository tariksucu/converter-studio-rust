//! Unit tests against a fake Converter Studio API (wiremock): no network, no API key.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::download::{safe_file_name, split_extension};
use crate::request::{api_error, retry_delay};
use crate::uploads::part_ranges;
use crate::{
    ApiErrorKind, Client, ConversionRequest, ConvertParams, DownloadOptions, Error, Status, VERSION,
};

const KEY: &str = "cvs_live_test_key";

type Sleeps = Arc<Mutex<Vec<Duration>>>;

/// A client for the fake API whose pauses are recorded instead of waited for.
fn client_for(server: &MockServer) -> (Client, Sleeps) {
    client_with(server, Client::builder().api_key(KEY))
}

fn client_with(server: &MockServer, builder: crate::ClientBuilder) -> (Client, Sleeps) {
    let sleeps: Sleeps = Arc::default();
    let recorded = Arc::clone(&sleeps);
    let client = builder
        .base_url(format!("{}/api", server.uri()))
        .build()
        .unwrap()
        .with_sleep(move |d| {
            recorded.lock().unwrap().push(d);
            Box::pin(async {})
        });
    (client, sleeps)
}

fn conversion(server: &MockServer, id: &str, status: &str) -> Value {
    let done = status == "done";
    json!({
        "id": id,
        "status": status,
        "from": "heic",
        "to": "jpg",
        "inputName": "IMG_1234.HEIC",
        "inputSize": 10,
        "credits": 1,
        "maxCredits": 20,
        "progress": if done { 1.0 } else { 0.5 },
        "outputName": if done { json!("IMG_1234.jpg") } else { Value::Null },
        "outputSize": if done { json!(4) } else { Value::Null },
        "downloadUrl": if done {
            json!(format!("{}/api/jobs/{id}/download?token=dl_secret", server.uri()))
        } else {
            Value::Null
        },
        "expiresAt": "2026-11-23T20:00:00.000Z",
        "error": null,
        "next": "Download the result."
    })
}

fn upload_slot(server: &MockServer, id: &str, part_bytes: u64) -> Value {
    json!({
        "id": id,
        "uploadUrl": format!("{}/api/v1/uploads/{id}?token=up_secret", server.uri()),
        "method": "PUT",
        "maxBytes": 1_000_000_000,
        "partBytes": part_bytes,
        "expiresAt": "2026-11-23T15:00:00.000Z",
        "curl": "curl ..."
    })
}

/// Mounts a whole successful conversion of an upload: slot, part(s), conversion, a long poll
/// that answers "done" and the download.
async fn mount_happy_path(server: &MockServer, part_bytes: u64) {
    Mock::given(method("POST"))
        .and(path("/api/v1/uploads"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(upload_slot(server, "up1", part_bytes)),
        )
        .mount(server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/uploads/up1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": "up1", "status": "ready"})),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/conversions"))
        .respond_with(ResponseTemplate::new(201).set_body_json(conversion(server, "up1", "queued")))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/conversions/up1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(conversion(server, "up1", "done")))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/jobs/up1/download"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(&b"\xff\xd8\xff\xe0"[..]))
        .mount(server)
        .await;
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap()
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request.headers.get(name).and_then(|v| v.to_str().ok())
}

fn body_json(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

/// A folder of its own under the system's temporary folder, deleted when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "converter-studio-test-{:016x}",
            crate::request::random_u64()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn converts_a_file_from_upload_to_download() {
    let server = MockServer::start().await;
    mount_happy_path(&server, 95 << 20).await;
    let (client, sleeps) = client_for(&server);
    let dir = TempDir::new();
    let source = dir.path().join("IMG_1234.HEIC");
    std::fs::write(&source, b"0123456789").unwrap();

    let params = ConvertParams::new()
        .option("quality", "high")
        .max_credits(5);
    let done = client.convert_file(&source, ".JPG", params).await.unwrap();
    assert!(done.is_done());
    assert_eq!(done.credits, Some(1));
    assert_eq!(done.output_name.as_deref(), Some("IMG_1234.jpg"));
    assert!(!done.replayed);
    assert_eq!(done.raw["next"], "Download the result.");

    let out = dir.path().join("out");
    let first = client
        .download_file(&done, format!("{}/", out.display()))
        .await
        .unwrap();
    let second = client.download_file(&done, &out).await.unwrap();
    assert_eq!(first, out.join("IMG_1234.jpg"));
    assert_eq!(second, out.join("IMG_1234-1.jpg"));
    assert_eq!(std::fs::read(&first).unwrap(), b"\xff\xd8\xff\xe0");
    assert_eq!(names_in(&out), ["IMG_1234-1.jpg", "IMG_1234.jpg"]);
    assert_eq!(
        client.download_bytes(&done).await.unwrap(),
        b"\xff\xd8\xff\xe0"
    );
    assert!(sleeps.lock().unwrap().is_empty());

    let seen = requests(&server).await;
    for request in &seen {
        let expected_client = format!("rust/{VERSION}");
        assert_eq!(
            header(request, "x-converter-studio-client"),
            Some(expected_client.as_str())
        );
        assert!(
            header(request, "user-agent")
                .unwrap()
                .starts_with("converter-studio-rust/")
        );
        // The key goes to the API only: not with the file (upload link) or the result.
        let is_api = request.url.path().starts_with("/api/v1/") && request.method.as_str() != "PUT";
        let expected_auth = format!("Bearer {KEY}");
        assert_eq!(
            header(request, "authorization"),
            is_api.then_some(expected_auth.as_str()),
            "{} {}",
            request.method,
            request.url.path()
        );
    }
    let methods: Vec<String> = seen
        .iter()
        .map(|r| format!("{} {}", r.method, r.url.path()))
        .collect();
    assert_eq!(
        &methods[..4],
        [
            "POST /api/v1/uploads",
            "PUT /api/v1/uploads/up1",
            "POST /api/v1/conversions",
            "GET /api/v1/conversions/up1",
        ]
    );
    assert_eq!(
        body_json(&seen[0]),
        json!({"fileName": "IMG_1234.HEIC", "size": 10})
    );
    assert_eq!(seen[1].body, b"0123456789");
    assert_eq!(header(&seen[1], "content-length"), Some("10"));
    assert_eq!(
        header(&seen[1], "content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(header(&seen[1], "upload-offset"), None);
    assert_eq!(
        body_json(&seen[2]),
        json!({"to": "jpg", "uploadId": "up1", "options": {"quality": "high"}, "maxCredits": 5})
    );
    assert!(
        header(&seen[2], "idempotency-key")
            .unwrap()
            .starts_with("cvs-rust-")
    );
    assert_eq!(seen[3].url.query(), Some("wait=25"));
}

#[tokio::test]
async fn sends_large_files_in_parts() {
    let server = MockServer::start().await;
    mount_happy_path(&server, 95 << 20).await;
    let (client, _) = client_with(&server, Client::builder().api_key(KEY).part_size(4));
    let upload = client
        .upload_bytes(&b"0123456789"[..], "notes.txt")
        .await
        .unwrap();
    assert_eq!(upload.id, "up1");

    let parts: Vec<Request> = requests(&server)
        .await
        .into_iter()
        .filter(|r| r.method.as_str() == "PUT")
        .collect();
    let seen: Vec<(Option<&str>, &[u8])> = parts
        .iter()
        .map(|r| (header(r, "upload-offset"), r.body.as_slice()))
        .collect();
    assert_eq!(
        seen,
        [
            (Some("0"), &b"0123"[..]),
            (Some("4"), &b"4567"[..]),
            (Some("8"), &b"89"[..])
        ]
    );
}

#[tokio::test]
async fn sends_a_large_local_file_in_parts_of_the_apis_size() {
    let server = MockServer::start().await;
    mount_happy_path(&server, 100_000).await;
    let (client, _) = client_for(&server);
    let dir = TempDir::new();
    let source = dir.path().join("big.wav");
    let data: Vec<u8> = (0..250_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&source, &data).unwrap();
    client.upload_file(&source).await.unwrap();

    let parts: Vec<Request> = requests(&server)
        .await
        .into_iter()
        .filter(|r| r.method.as_str() == "PUT")
        .collect();
    let joined: Vec<u8> = parts.iter().flat_map(|r| r.body.clone()).collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(header(&parts[2], "upload-offset"), Some("200000"));
    assert_eq!(header(&parts[2], "content-length"), Some("50000"));
    assert_eq!(joined, data);
}

#[tokio::test]
async fn retries_with_the_same_idempotency_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/conversions"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("Retry-After", "2")
                .set_body_json(json!({"error": {"code": "serverBusy", "message": "Busy."}})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/conversions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Idempotent-Replayed", "true")
                .set_body_json(conversion(&server, "c1", "queued")),
        )
        .mount(&server)
        .await;
    let (client, sleeps) = client_for(&server);
    let created = client
        .create_conversion(&ConversionRequest::from_url(
            "https://example.com/a.heic",
            "jpg",
        ))
        .await
        .unwrap();
    assert!(created.replayed);
    assert_eq!(created.status, Status::Queued);
    assert_eq!(*sleeps.lock().unwrap(), [Duration::from_secs(2)]);
    let seen = requests(&server).await;
    assert_eq!(seen.len(), 2);
    assert_eq!(
        header(&seen[0], "idempotency-key"),
        header(&seen[1], "idempotency-key")
    );
    assert_eq!(
        body_json(&seen[0]),
        json!({"to": "jpg", "url": "https://example.com/a.heic"})
    );
}

#[tokio::test]
async fn keeps_the_callers_idempotency_key_and_reports_a_reused_one() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/conversions"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({"error": {
            "code": "idempotencyKeyReused",
            "message": "This Idempotency-Key was already used for a different request."
        }})))
        .mount(&server)
        .await;
    let (client, sleeps) = client_for(&server);
    let request = ConversionRequest::from_upload("u1", "png")
        .idempotency_key("job-42")
        .callback_url("https://example.com/hook?token=t")
        .quote_only();
    let err = client.create_conversion(&request).await.unwrap_err();
    assert_eq!(err.api_kind(), Some(ApiErrorKind::Conflict));
    assert_eq!(err.code(), Some("idempotencyKeyReused"));
    assert_eq!(
        err.to_string(),
        "This Idempotency-Key was already used for a different request."
    );
    assert!(sleeps.lock().unwrap().is_empty());
    let seen = requests(&server).await;
    assert_eq!(header(&seen[0], "idempotency-key"), Some("job-42"));
    assert_eq!(
        body_json(&seen[0]),
        json!({"to": "png", "uploadId": "u1", "maxCredits": 0,
               "callbackUrl": "https://example.com/hook?token=t"})
    );

    let bad = ConversionRequest::from_upload("u1", "png").idempotency_key("with space");
    assert!(matches!(
        client.create_conversion(&bad).await,
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(requests(&server).await.len(), 1);
}

#[tokio::test]
async fn does_not_retry_a_wrong_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": {
            "code": "invalidApiKey",
            "message": "This API key does not work. Create one on your account page at converter.studio."
        }})))
        .mount(&server)
        .await;
    let (client, sleeps) = client_for(&server);
    let err = client.account().await.unwrap_err();
    let Error::Api(api) = &err else {
        panic!("{err:?}")
    };
    assert_eq!(api.status, 401);
    assert_eq!(api.kind(), ApiErrorKind::Authentication);
    assert_eq!(api.code.as_deref(), Some("invalidApiKey"));
    assert!(err.to_string().starts_with("This API key does not work."));
    assert!(sleeps.lock().unwrap().is_empty());
    assert_eq!(requests(&server).await.len(), 1);
}

#[tokio::test]
async fn gives_up_after_the_retries_on_a_proxy_error_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/formats"))
        .respond_with(ResponseTemplate::new(502).set_body_string("<html>Bad gateway</html>"))
        .mount(&server)
        .await;
    let (client, sleeps) = client_for(&server);
    let err = client.formats().await.unwrap_err();
    assert_eq!(err.api_kind(), Some(ApiErrorKind::Server));
    assert_eq!(err.code(), None);
    assert_eq!(
        err.to_string(),
        "The Converter Studio API answered 502 Bad Gateway."
    );
    assert_eq!(requests(&server).await.len(), 3);
    let sleeps = sleeps.lock().unwrap().clone();
    assert_eq!(sleeps.len(), 2);
    assert!((Duration::from_millis(250)..=Duration::from_millis(500)).contains(&sleeps[0]));
    assert!((Duration::from_millis(500)..=Duration::from_millis(1000)).contains(&sleeps[1]));
}

#[tokio::test]
async fn retries_connection_errors_and_names_only_the_host() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let sleeps: Sleeps = Arc::default();
    let recorded = Arc::clone(&sleeps);
    let client = Client::builder()
        .api_key(KEY)
        .base_url(format!("http://127.0.0.1:{port}/api"))
        .build()
        .unwrap()
        .with_sleep(move |d| {
            recorded.lock().unwrap().push(d);
            Box::pin(async {})
        });
    let err = client.account().await.unwrap_err();
    let Error::Connection(connection) = &err else {
        panic!("{err:?}")
    };
    assert_eq!(connection.host, format!("127.0.0.1:{port}"));
    assert!(!connection.timeout);
    assert!(
        err.to_string()
            .starts_with(&format!("Could not reach 127.0.0.1:{port}"))
    );
    assert_eq!(sleeps.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn times_out_a_request_without_progress() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"email": "a@example.com"}))
                .set_delay(Duration::from_secs(3)),
        )
        .mount(&server)
        .await;
    let (client, _) = client_with(
        &server,
        Client::builder()
            .api_key(KEY)
            .timeout(Duration::from_millis(300))
            .max_retries(0),
    );
    let started = Instant::now();
    let err = client.account().await.unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let Error::Connection(connection) = &err else {
        panic!("{err:?}")
    };
    assert!(connection.timeout);
    assert!(err.to_string().ends_with("timed out."), "{err}");
}

#[tokio::test]
async fn a_long_poll_may_take_longer_than_the_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/conversions/c1"))
        .and(query_param("wait", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(conversion(&server, "c1", "done"))
                .set_delay(Duration::from_millis(700)),
        )
        .mount(&server)
        .await;
    let (client, _) = client_with(
        &server,
        Client::builder()
            .api_key(KEY)
            .timeout(Duration::from_millis(400))
            .max_retries(0),
    );
    let done = client
        .wait_for_update("c1", Duration::from_secs(1))
        .await
        .unwrap();
    assert!(done.is_done());
}

#[tokio::test]
async fn waits_with_growing_pauses_when_answers_come_back_early() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/conversions/c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(conversion(
            &server,
            "c1",
            "processing",
        )))
        .up_to_n_times(5)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/conversions/c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(conversion(&server, "c1", "done")))
        .mount(&server)
        .await;
    let (client, sleeps) = client_for(&server);
    let done = client.wait("c1").await.unwrap();
    assert!(done.is_done());
    let seconds: Vec<u64> = sleeps
        .lock()
        .unwrap()
        .iter()
        .map(Duration::as_secs)
        .collect();
    assert_eq!(seconds, [1, 2, 4, 8]);
    let seen = requests(&server).await;
    assert_eq!(seen[0].url.query(), None);
    assert!(seen[1..].iter().all(|r| r.url.query() == Some("wait=25")));
}

#[tokio::test]
async fn gives_up_waiting_after_the_wait_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/conversions/c1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(conversion(&server, "c1", "processing"))
                .set_delay(Duration::from_millis(200)),
        )
        .mount(&server)
        .await;
    let (client, _) = client_with(
        &server,
        Client::builder()
            .api_key(KEY)
            .wait_timeout(Duration::from_secs(1)),
    );
    let started = Instant::now();
    let err = client.wait("c1").await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(3));
    let Error::WaitTimeout(timeout) = &err else {
        panic!("{err:?}")
    };
    assert_eq!(timeout.timeout, Duration::from_secs(1));
    assert_eq!(timeout.conversion.status, Status::Processing);
    assert!(err.to_string().contains("look it up later by its id, c1"));
    let seen = requests(&server).await;
    assert!(seen[1..].iter().all(|r| r.url.query() == Some("wait=1")));
}

#[tokio::test]
async fn reports_failed_expired_and_canceled_conversions() {
    let server = MockServer::start().await;
    let mut failed = conversion(&server, "f1", "failed");
    failed["error"] = json!({
        "code": "conversionFailed",
        "message": "We couldn't convert this file. Your credits were given back.",
        "params": {"detail": 1}
    });
    let cases = [
        (
            "f1",
            failed,
            "conversionFailed",
            "Your credits were given back.",
        ),
        (
            "e1",
            conversion(&server, "e1", "expired"),
            "fileExpired",
            "deleted after its time",
        ),
        (
            "x1",
            conversion(&server, "x1", "canceled"),
            "canceled",
            "nothing was charged",
        ),
    ];
    for (id, body, _, _) in &cases {
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/conversions/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body.clone()))
            .mount(&server)
            .await;
    }
    let (client, _) = client_for(&server);
    for (id, _, code, text) in cases {
        let err = client.wait(id).await.unwrap_err();
        let Error::ConversionFailed(failure) = &err else {
            panic!("{err:?}")
        };
        assert_eq!(failure.code, code);
        assert_eq!(err.code(), Some(code));
        assert!(err.to_string().contains(text), "{err}");
        assert_eq!(err.conversion().unwrap().id, id);
        // Nothing to download.
        let dir = TempDir::new();
        assert!(matches!(
            client.download_file(&failure.conversion, dir.path()).await,
            Err(Error::ConversionFailed(_))
        ));
        assert_eq!(names_in(dir.path()), Vec::<String>::new());
    }
}

#[tokio::test]
async fn a_dear_conversion_waits_for_confirm() {
    let server = MockServer::start().await;
    let mut quoted = conversion(&server, "q1", "quoted");
    quoted["credits"] = json!(7);
    quoted["maxCredits"] = json!(5);
    Mock::given(method("POST"))
        .and(path("/api/v1/conversions"))
        .respond_with(ResponseTemplate::new(201).set_body_json(quoted))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/conversions/q1/confirm"))
        .respond_with(ResponseTemplate::new(200).set_body_json(conversion(&server, "q1", "queued")))
        .mount(&server)
        .await;
    let (client, _) = client_for(&server);
    let err = client
        .convert_url(
            "https://example.com/talk.mkv",
            "mp4",
            ConvertParams::new().max_credits(5),
        )
        .await
        .unwrap_err();
    let Error::ConfirmationRequired(quote) = &err else {
        panic!("{err:?}")
    };
    assert_eq!((quote.credits, quote.max_credits), (Some(7), 5));
    assert!(quote.conversion.needs_confirmation());
    assert_eq!(
        err.to_string(),
        "This conversion costs 7 credits, more than the 5 allowed to go ahead on their own. \
         Confirm it to start, or leave it: nothing has been charged."
    );
    let confirmed = client.confirm("q1").await.unwrap();
    assert_eq!(confirmed.status, Status::Queued);
}

#[tokio::test]
async fn returns_at_once_with_no_wait() {
    let server = MockServer::start().await;
    mount_happy_path(&server, 95 << 20).await;
    let (client, _) = client_for(&server);
    let created = client
        .convert_bytes(&b"abc"[..], "a.heic", "jpg", ConvertParams::new().no_wait())
        .await
        .unwrap();
    assert_eq!(created.status, Status::Queued);
    assert!(matches!(
        client.download_bytes(&created).await,
        Err(Error::NotDone(_))
    ));
    let seen = requests(&server).await;
    assert!(seen.iter().all(|r| r.method.as_str() != "GET"));
}

#[tokio::test]
async fn a_deleted_result_leaves_no_file() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/jobs/d1/download"))
        .respond_with(ResponseTemplate::new(410).set_body_json(json!({"error": "fileExpired"})))
        .mount(&server)
        .await;
    let (client, _) = client_for(&server);
    let dir = TempDir::new();
    let done: crate::Conversion =
        serde_json::from_value(conversion(&server, "d1", "done")).unwrap();
    let err = client.download_file(&done, dir.path()).await.unwrap_err();
    assert_eq!(err.api_kind(), Some(ApiErrorKind::Gone));
    assert_eq!(err.code(), Some("fileExpired"));
    assert_eq!(
        err.to_string(),
        "This file has already been deleted. Please convert it again."
    );
    assert_eq!(names_in(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn saves_under_a_safe_name_and_never_replaces_unless_asked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/jobs/s1/download"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(&b"new"[..]))
        .mount(&server)
        .await;
    let (client, _) = client_for(&server);
    let dir = TempDir::new();
    let mut done: crate::Conversion =
        serde_json::from_value(conversion(&server, "s1", "done")).unwrap();
    done.output_name = Some("../../etc/pass\u{7}wd.txt".to_owned());
    let saved = client.download_file(&done, dir.path()).await.unwrap();
    assert_eq!(saved, dir.path().join("pass_wd.txt"));

    let exact = dir.path().join("mine.txt");
    std::fs::write(&exact, b"old").unwrap();
    let err = client.download_file(&done, &exact).await.unwrap_err();
    assert!(matches!(&err, Error::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists));
    assert_eq!(std::fs::read(&exact).unwrap(), b"old");
    let replaced = client
        .download_file_with(&done, &exact, DownloadOptions::new().overwrite(true))
        .await
        .unwrap();
    assert_eq!(replaced, exact);
    assert_eq!(std::fs::read(&exact).unwrap(), b"new");
    assert_eq!(names_in(dir.path()), ["mine.txt", "pass_wd.txt"]);
}

#[tokio::test]
async fn a_part_whose_answer_was_lost_counts_as_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/uploads"))
        .respond_with(ResponseTemplate::new(201).set_body_json(upload_slot(&server, "up1", 1000)))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/uploads/up1"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/uploads/up1"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error": {"code": "notFound", "message": "Gone."}})),
        )
        .mount(&server)
        .await;
    let (client, sleeps) = client_for(&server);
    let upload = client.upload_bytes(&b"abc"[..], "a.png").await.unwrap();
    assert_eq!(upload.id, "up1");
    assert_eq!(sleeps.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn lists_formats_without_a_key() {
    let server = MockServer::start().await;
    let heic = json!({"id": "heic", "name": "HEIC", "category": "image",
                      "extensions": ["heic", "heif"], "to": ["jpg", "png"]});
    Mock::given(method("GET"))
        .and(path("/api/v1/formats"))
        .and(query_param("from", "heic"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"formats": [heic]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/formats"))
        .and(query_param("from", "txt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"formats": []})))
        .mount(&server)
        .await;
    let (client, _) = client_with(&server, Client::builder().api_key(""));
    let found = client.format(".HEIC").await.unwrap().unwrap();
    assert_eq!(found.targets, ["jpg", "png"]);
    assert_eq!(found.extensions, ["heic", "heif"]);
    assert_eq!(client.format("txt").await.unwrap(), None);
    assert!(
        requests(&server)
            .await
            .iter()
            .all(|r| header(r, "authorization").is_none())
    );

    assert!(matches!(client.account().await, Err(Error::MissingApiKey)));
    assert!(matches!(
        client
            .convert_url("https://example.com/a.png", "jpg", ConvertParams::new())
            .await,
        Err(Error::MissingApiKey)
    ));
    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn reads_the_account() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "email": "a@example.com", "plan": "pro", "allApps": true, "credits": 990,
            "allowance": {"used": 10, "limit": 1000, "per": "month"}, "packs": 0,
            "renewsAt": "2026-12-23T00:00:00.000Z", "maxFileBytes": 2_000_000_000u64,
            "maxMediaSeconds": 14_400, "wallet": null, "future": "field"
        })))
        .mount(&server)
        .await;
    let (client, _) = client_for(&server);
    let account = client.account().await.unwrap();
    assert_eq!(account.plan, "pro");
    assert!(account.all_apps);
    assert_eq!(account.credits, 990);
    assert_eq!(account.allowance.unwrap().per, "month");
    assert_eq!(account.max_file_bytes, 2_000_000_000);
    assert!(account.wallet.is_none());
    assert_eq!(account.raw["future"], "field");
}

#[tokio::test]
async fn checks_arguments_before_any_request() {
    let server = MockServer::start().await;
    let (client, _) = client_for(&server);
    assert!(matches!(
        client
            .convert_url("https://example.com/a.png", " ", ConvertParams::new())
            .await,
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        client
            .convert_file("no-such-file.heic", "", ConvertParams::new())
            .await,
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        client
            .convert_file("no-such-file.heic", "jpg", ConvertParams::new())
            .await,
        Err(Error::Io(_))
    ));
    assert!(matches!(
        client.conversion("").await,
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        client
            .create_conversion(&ConversionRequest::from_url("", "jpg"))
            .await,
        Err(Error::InvalidArgument(_))
    ));
    assert!(requests(&server).await.is_empty());
}

#[test]
fn checks_the_settings_of_the_builder() {
    let client = Client::builder()
        .api_key("k")
        .base_url("https://converter.studio/api/")
        .build()
        .unwrap();
    assert_eq!(client.base_url(), "https://converter.studio/api");
    assert_eq!(
        client.api_url(&["v1", "conversions", "a/b c"]).as_str(),
        "https://converter.studio/api/v1/conversions/a%2Fb%20c"
    );
    let root = Client::builder()
        .api_key("k")
        .base_url("http://localhost:1")
        .build()
        .unwrap();
    assert_eq!(
        root.api_url(&["v1", "formats"]).as_str(),
        "http://localhost:1/v1/formats"
    );
    for wrong in [
        "ftp://example.com",
        "example.com",
        "https://x/api?a=1",
        "https://u:p@x/api",
    ] {
        assert!(
            matches!(
                Client::builder().base_url(wrong).build(),
                Err(Error::Config(_))
            ),
            "{wrong}"
        );
    }
    assert!(matches!(
        Client::builder().user_agent("a\nb").build(),
        Err(Error::Config(_))
    ));
    assert!(matches!(
        Client::builder().timeout(Duration::ZERO).build(),
        Err(Error::Config(_))
    ));
}

/// The client can be shared between tasks, its futures can be spawned, and its error works
/// with `Box<dyn Error + Send + Sync>` (anyhow and friends). Checked when it compiles.
#[allow(dead_code)]
fn futures_and_errors_can_cross_threads(client: &Client, writer: &mut tokio::io::Sink) {
    fn send<T: Send>(_: T) {}
    fn shared<T: Send + Sync + Clone + 'static>() {}
    fn error<T: std::error::Error + Send + Sync + 'static>() {}
    shared::<Client>();
    error::<Error>();
    let conversion = crate::Conversion::default();
    send(client.convert_file("a", "b", ConvertParams::new()));
    send(client.convert_bytes(Vec::new(), "a", "b", ConvertParams::new()));
    send(client.convert_url("a", "b", ConvertParams::new()));
    send(client.wait("a"));
    send(client.formats());
    send(client.account());
    send(client.download(&conversion, writer));
    send(client.download_file(&conversion, "a"));
}

#[test]
fn keeps_secrets_out_of_debug_output() {
    let client = Client::builder().api_key(KEY).build().unwrap();
    let printed = format!("{client:?} {:?}", Client::builder().api_key(KEY));
    assert!(!printed.contains(KEY), "{printed}");
    let conversion: crate::Conversion = serde_json::from_value(json!({
        "id": "c1", "status": "done", "downloadUrl": "https://x/dl?token=dl_secret"
    }))
    .unwrap();
    let upload: crate::Upload = serde_json::from_value(json!({
        "id": "u1", "uploadUrl": "https://x/up?token=up_secret"
    }))
    .unwrap();
    let printed = format!("{conversion:?} {upload:?}");
    assert!(!printed.contains("secret"), "{printed}");
    assert!(printed.contains("<redacted>"));
}

#[test]
fn reads_unknown_statuses_and_missing_fields() {
    let conversion: crate::Conversion =
        serde_json::from_value(json!({"id": "c1", "status": "teleporting"})).unwrap();
    assert_eq!(conversion.status, Status::Unknown);
    assert!(!conversion.is_finished());
    assert_eq!(Status::WaitingForUpload.to_string(), "waiting_for_upload");
}

#[test]
fn reads_error_answers() {
    let e = api_error(
        reqwest::StatusCode::PAYMENT_REQUIRED,
        br#"{"error":{"code":"notEnoughCredits","message":"Not enough credits.","params":{"needed":3,"balance":1}}}"#,
        None,
    );
    assert_eq!(e.kind(), ApiErrorKind::InsufficientCredits);
    assert_eq!(e.params["needed"], 3);
    let e = api_error(
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        br#"{"error":"tooManyRequests"}"#,
        Some(Duration::from_secs(5)),
    );
    assert_eq!(e.kind(), ApiErrorKind::RateLimited);
    assert!(e.message.starts_with("You're going a bit fast."));
    assert_eq!(e.retry_after, Some(Duration::from_secs(5)));
    let e = api_error(reqwest::StatusCode::IM_A_TEAPOT, b"", None);
    assert_eq!(e.kind(), ApiErrorKind::Other);
    assert_eq!(
        e.message,
        "The Converter Studio API answered 418 I'm a teapot."
    );
}

#[test]
fn pauses_between_retries() {
    assert_eq!(
        retry_delay(0, Some(Duration::from_secs(300))),
        Duration::from_secs(60)
    );
    for attempt in 0..8 {
        let base = Duration::from_millis(500) * (1 << attempt.min(4));
        let base = base.min(Duration::from_secs(8));
        let pause = retry_delay(attempt, None);
        assert!(pause >= base / 2 && pause <= base, "{attempt}: {pause:?}");
    }
}

#[test]
fn splits_files_into_parts() {
    assert_eq!(part_ranges(0, 10), [(0, 0)]);
    assert_eq!(part_ranges(10, 10), [(0, 10)]);
    assert_eq!(part_ranges(25, 10), [(0, 10), (10, 10), (20, 5)]);
}

#[test]
fn makes_safe_file_names() {
    assert_eq!(safe_file_name("a/b\\c.jpg", "x"), "c.jpg");
    assert_eq!(safe_file_name("..", "x.bin"), "x.bin");
    assert_eq!(safe_file_name(" .hidden. ", "x"), "hidden");
    assert_eq!(split_extension("a.tar.gz"), ("a", ".tar.gz"));
    assert_eq!(split_extension("photo.jpg"), ("photo", ".jpg"));
    assert_eq!(split_extension(".profile"), (".profile", ""));
    assert_eq!(split_extension("README"), ("README", ""));
}

#[cfg(feature = "blocking")]
#[test]
fn the_blocking_client_converts_too() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = runtime.block_on(async {
        let server = MockServer::start().await;
        mount_happy_path(&server, 95 << 20).await;
        server
    });
    let client = crate::blocking::Client::from_builder(
        Client::builder()
            .api_key(KEY)
            .base_url(format!("{}/api", server.uri())),
    )
    .unwrap();
    let done = client
        .convert_bytes(
            &b"0123456789"[..],
            "IMG_1234.HEIC",
            "jpg",
            ConvertParams::new(),
        )
        .unwrap();
    let mut out = Vec::new();
    assert_eq!(client.download(&done, &mut out).unwrap(), 4);
    assert_eq!(out, b"\xff\xd8\xff\xe0");
    drop(runtime);
}
