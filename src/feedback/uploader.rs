// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

// Three-step submission orchestrator. Talks to niyien.com /api/feedback*,
// then handles the actual zip upload — branching on `upload.kind` between
// the two protocols documented in docs/feedback-schema.md §6:
//
// * `r2_presigned_put`  — single PUT with provided headers (global region).
// * `pan123_multipart`  — 123 OpenAPI 3-step (create / slice / complete),
//                         since 123 has no presigned PUT (cn region).
//
// Failure recovery: on PUT-failure / confirm-failure, the zip is persisted
// to `<data_dir>/feedback/pending/<id>.{zip,json}` and a startup hook
// (`retry_pending`) re-runs the appropriate stage.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::meta::Meta;
use super::packager::{self, PackageInputs, PackageOptions};

// -- public surface --------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum FeedbackJobState {
    Packaging,
    RequestingToken,
    Uploading { pct: u32 },
    Confirming,
    Cleanup,
    Done { id: String },
    Failed { reason: String, persisted: bool },
}

#[derive(Clone, Debug)]
pub struct JobOutcome {
    pub success: bool,
    pub id:      Option<String>,
    pub error:   Option<String>,
}

pub struct SubmitArgs {
    pub inputs:      PackageInputs,
    pub options:     PackageOptions,
    pub summary:     String,
    pub email:       String,
    pub meta:        Meta,
    pub events:      Sender<FeedbackJobState>,
}

/// Run the full submit pipeline synchronously. Caller is expected to have
/// spawned a worker thread for this. Emits `events` on each state change.
pub fn submit(args: SubmitArgs) -> JobOutcome {
    let SubmitArgs { inputs, options, summary, email, meta, events } = args;
    let _ = events.send(FeedbackJobState::Packaging);
    let (zip_bytes, sha256) = match packager::pack(&inputs, &options, &summary, &email, &meta) {
        Ok(v) => v,
        Err(e) => return fail(&events, &format!("pack: {e}"), false),
    };
    let size = zip_bytes.len() as u64;

    let _ = events.send(FeedbackJobState::RequestingToken);
    let token = match with_retry("request_token", || request_token(&meta, &summary, &email, size, &sha256)) {
        Ok(t) => t,
        Err(e) => return fail(&events, &format!("request_token: {e}"), false),
    };

    let id = token.id.clone();
    let _ = events.send(FeedbackJobState::Uploading { pct: 0 });

    let upload_result = match token.upload.kind.as_str() {
        "r2_presigned_put" => upload_r2(&token, &zip_bytes, &events),
        "pan123_multipart" => upload_pan123(&token, &zip_bytes, &events),
        other => Err(format!("unknown upload kind: {other}")),
    };
    if let Err(e) = upload_result {
        // Persist for startup retry.
        let _ = persist_pending(&id, &token, &zip_bytes, "uploading", &sha256);
        return fail(&events, &format!("upload: {e}"), true);
    }

    let _ = events.send(FeedbackJobState::Confirming);
    if let Err(e) = with_retry("confirm", || confirm(&id, size, &sha256)) {
        let _ = persist_pending(&id, &token, &zip_bytes, "confirming", &sha256);
        return fail(&events, &format!("confirm: {e}"), true);
    }

    let _ = events.send(FeedbackJobState::Cleanup);
    cleanup(&inputs, &options);

    let _ = events.send(FeedbackJobState::Done { id: id.clone() });
    JobOutcome { success: true, id: Some(id), error: None }
}

fn fail(events: &Sender<FeedbackJobState>, reason: &str, persisted: bool) -> JobOutcome {
    let _ = events.send(FeedbackJobState::Failed { reason: reason.to_string(), persisted });
    JobOutcome { success: false, id: None, error: Some(reason.to_string()) }
}

// -- HTTP helpers ----------------------------------------------------------

const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(600);

fn feedback_request<T>(request: ureq::RequestBuilder<T>, timeout: Duration) -> ureq::RequestBuilder<T> {
    // ureq also checks the response timeout while reading the body. Give each
    // feedback request one budget covering send, server processing, and receive.
    request.config()
        .timeout_recv_response(None)
        .timeout_recv_body(None)
        .timeout_global(Some(timeout))
        .build()
}

fn upload_request_with_retry<F>(name: &str, bytes: &[u8], timeout: Duration, mut request: F) -> Result<String, String>
where
    F: FnMut() -> ureq::RequestBuilder<ureq::typestate::WithBody>,
{
    let mut attempt = 0;
    loop {
        attempt += 1;
        let started = Instant::now();
        let result = feedback_request(request(), timeout).send(bytes).and_then(|mut response| {
            if !response.status().is_success() {
                return Err(ureq::Error::StatusCode(response.status().as_u16()));
            }
            response.body_mut().read_to_string()
        });
        match result {
            Ok(body) => {
                log::info!(target: "feedback", "{name} complete attempt={attempt} bytes={} elapsed_ms={}", bytes.len(), started.elapsed().as_millis());
                return Ok(body);
            }
            Err(error) => {
                log::warn!(target: "feedback", "{name} failed attempt={attempt} bytes={} elapsed_ms={} error={error:?}", bytes.len(), started.elapsed().as_millis());
                if attempt >= super::RETRY_ATTEMPTS || !crate::network::is_transient_error(&error) {
                    return Err(format!("{name}: {error}"));
                }
                let backoff = super::BACKOFF_SECS.get((attempt - 1) as usize).copied().unwrap_or(8);
                std::thread::sleep(Duration::from_secs(backoff));
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct UploadContract {
    method:  String,
    kind:    String,
    /// r2_presigned_put: full URL. pan123_multipart: open_api_base.
    #[serde(default)]
    url:     String,
    #[serde(default)]
    headers: HashMap<String, String>,

    // pan123_multipart-only fields
    #[serde(default)]
    open_api_base:  Option<String>,
    #[serde(default)]
    access_token:   Option<String>,
    #[serde(default)]
    parent_file_id: Option<i64>,
    #[serde(default)]
    parent_path:    Option<String>,
    #[serde(default)]
    filename:       Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TokenResponse {
    id:         String,
    region:     String,
    upload:     UploadContract,
    expires_at: String,
}

fn request_token(meta: &Meta, summary: &str, email: &str, size: u64, sha256: &str) -> Result<TokenResponse, String> {
    let body = serde_json::json!({
        "app_version": meta.app_version,
        "os":          meta.os,
        "gpu":         meta.gpu,
        "size":        size,
        "sha256":      sha256,
        "summary":     summary,
        "email":       email,
    });
    let url = format!("{}/feedback", super::NIYIEN_FEEDBACK_BASE);
    let resp = feedback_request(crate::network::post(&url), API_REQUEST_TIMEOUT)
        .header("Content-Type", "application/json")
        .send(serde_json::to_string(&body).map_err(|e| format!("encode: {e}"))?.as_str())
        .map_err(|e| format!("HTTP error: {e}"))?;
    let status = resp.status();
    if status != 200 {
        return Err(format!("status {status}"));
    }
    let body_text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read response: {e}"))?;
    let parsed: TokenResponse = serde_json::from_str(&body_text)
        .map_err(|e| format!("decode response: {e}"))?;
    Ok(parsed)
}

fn upload_r2(token: &TokenResponse, bytes: &[u8], events: &Sender<FeedbackJobState>) -> Result<(), String> {
    let upload = &token.upload;
    let _ = events.send(FeedbackJobState::Uploading { pct: 10 });
    let _ = events.send(FeedbackJobState::Uploading { pct: 30 });
    // Retrying the same signed PUT replaces the same object with identical bytes.
    upload_request_with_retry("R2 PUT", bytes, UPLOAD_TIMEOUT, || {
        let mut request = crate::network::put(&upload.url);
        for (k, v) in &upload.headers {
            request = request.header(k.as_str(), v.as_str());
        }
        request
    })?;
    let _ = events.send(FeedbackJobState::Uploading { pct: 100 });
    Ok(())
}

// -- 123 OpenAPI multipart -------------------------------------------------

#[derive(Deserialize)]
struct Pan123Resp<T> {
    code:    i32,
    #[serde(default)]
    message: String,
    #[serde(default)]
    data:    Option<T>,
}

#[derive(Deserialize, Default)]
struct Pan123CreateData {
    #[serde(rename = "preuploadID")]
    preupload_id: String,
    #[serde(rename = "sliceSize", default)]
    slice_size:   i64,
    #[serde(default)]
    servers:      Vec<String>,
    #[serde(default)]
    reuse:        bool,
    #[serde(rename = "fileID", default)]
    #[allow(dead_code)]
    file_id:      i64,
}

#[derive(Deserialize, Default)]
struct Pan123CompleteData {
    #[serde(default)]
    completed: bool,
    #[serde(rename = "fileID", default)]
    file_id:   i64,
}

fn upload_pan123(token: &TokenResponse, bytes: &[u8], events: &Sender<FeedbackJobState>) -> Result<(), String> {
    let upload = &token.upload;
    let base = upload.open_api_base.as_deref()
        .ok_or("pan123: missing open_api_base")?;
    let access = upload.access_token.as_deref()
        .ok_or("pan123: missing access_token")?;
    let parent_id = upload.parent_file_id
        .ok_or("pan123: missing parent_file_id")?;
    let filename = upload.filename.as_deref()
        .ok_or("pan123: missing filename")?;
    let etag = md5_hex(bytes);

    // 1. Create
    let create_body = serde_json::json!({
        "parentFileID": parent_id,
        "filename":     filename,
        "etag":         etag,
        "size":         bytes.len() as i64,
        "duplicate":    2,
    });
    let create: Pan123CreateData = pan123_post(&format!("{base}/upload/v2/file/create"), access, &create_body)?;
    if create.reuse {
        // Server already has this file — done.
        let _ = events.send(FeedbackJobState::Uploading { pct: 100 });
        return Ok(());
    }
    if create.preupload_id.is_empty() || create.slice_size <= 0 {
        return Err("pan123: invalid create response".into());
    }
    let mut servers = create.servers.clone();
    if servers.is_empty() {
        // fallback: query domain endpoint
        let domains: Vec<String> = pan123_get(&format!("{base}/upload/v2/file/domain"), access)?;
        servers = domains;
    }
    if servers.is_empty() {
        return Err("pan123: no upload servers available".into());
    }

    // 2. Slices
    let slice_size = create.slice_size as usize;
    let total_slices = ((bytes.len() + slice_size - 1) / slice_size).max(1);
    let mut slice_no: usize = 0;
    let mut offset: usize = 0;
    while offset < bytes.len() {
        let end = (offset + slice_size).min(bytes.len());
        let chunk = &bytes[offset..end];
        slice_no += 1;
        let server = servers[(slice_no - 1) % servers.len()].trim_end_matches('/');
        let url = format!("{server}/upload/v2/file/slice");
        let slice_md5 = md5_hex(chunk);
        upload_pan123_slice(&url, access, &create.preupload_id, slice_no, &slice_md5, chunk, filename)?;

        let pct = (((slice_no as f64) / (total_slices as f64)) * 90.0) as u32;
        let _ = events.send(FeedbackJobState::Uploading { pct: pct.min(95) });
        offset = end;
    }

    // 3. Complete (poll up to 120s)
    let complete_url = format!("{base}/upload/v2/file/upload_complete");
    for attempt in 1..=120 {
        match pan123_post::<Pan123CompleteData>(&complete_url, access, &serde_json::json!({"preuploadID": create.preupload_id})) {
            Ok(c) if c.completed && c.file_id > 0 => {
                let _ = events.send(FeedbackJobState::Uploading { pct: 100 });
                return Ok(());
            }
            Ok(_) => { /* still hashing server-side, retry */ }
            Err(e) => {
                if attempt == 120 {
                    return Err(format!("pan123: complete failed after 120 attempts: {e}"));
                }
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err("pan123: complete timed out".into())
}

fn upload_pan123_slice(
    url: &str,
    access: &str,
    preupload_id: &str,
    slice_no: usize,
    slice_md5: &str,
    chunk: &[u8],
    filename: &str,
) -> Result<(), String> {
    // Build multipart/form-data manually. ureq 3.x doesn't ship a form-data
    // builder, but the protocol is small enough to assemble by hand.
    let boundary = format!("----GyroflowFB{}", uuid::Uuid::new_v4().simple());
    let mut body: Vec<u8> = Vec::with_capacity(chunk.len() + 1024);
    let part_filename = format!("{filename}.part{slice_no}");
    let prelude = |name: &str| format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n");
    write!(body, "{}", prelude("preuploadID")).unwrap();
    body.extend_from_slice(preupload_id.as_bytes());
    body.extend_from_slice(b"\r\n");
    write!(body, "{}", prelude("sliceNo")).unwrap();
    body.extend_from_slice(slice_no.to_string().as_bytes());
    body.extend_from_slice(b"\r\n");
    write!(body, "{}", prelude("sliceMD5")).unwrap();
    body.extend_from_slice(slice_md5.as_bytes());
    body.extend_from_slice(b"\r\n");
    write!(body, "--{boundary}\r\nContent-Disposition: form-data; name=\"slice\"; filename=\"{part_filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").unwrap();
    body.extend_from_slice(chunk);
    body.extend_from_slice(b"\r\n");
    write!(body, "--{boundary}--\r\n").unwrap();

    // Keep the upload ID, slice number, checksum, and multipart body on retries.
    let body_text = upload_request_with_retry(&format!("slice {slice_no}"), &body, UPLOAD_TIMEOUT, || {
        crate::network::post(url)
            .header("Authorization", &format!("Bearer {access}"))
            .header("Platform", "open_platform")
            .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
    })?;
    let payload: Pan123Resp<serde_json::Value> = serde_json::from_str(&body_text)
        .map_err(|e| format!("slice {slice_no} decode: {e}"))?;
    if payload.code != 0 {
        return Err(format!("slice {slice_no} api code={} msg={}", payload.code, payload.message));
    }
    Ok(())
}

fn pan123_post<T: for<'de> Deserialize<'de> + Default>(url: &str, access: &str, body: &serde_json::Value) -> Result<T, String> {
    let body_str = serde_json::to_string(body).map_err(|e| format!("encode: {e}"))?;
    let resp = feedback_request(crate::network::post(url), API_REQUEST_TIMEOUT)
        .header("Authorization", &format!("Bearer {access}"))
        .header("Content-Type", "application/json")
        .header("Platform", "open_platform")
        .send(body_str.as_str())
        .map_err(|e| format!("POST {url}: {e}"))?;
    let body_text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read {url}: {e}"))?;
    let payload: Pan123Resp<T> = serde_json::from_str(&body_text)
        .map_err(|e| format!("decode {url}: {e}"))?;
    if payload.code != 0 {
        return Err(format!("api code={} msg={}", payload.code, payload.message));
    }
    payload.data.ok_or_else(|| "api returned no data".to_string())
}

fn pan123_get<T: for<'de> Deserialize<'de> + Default>(url: &str, access: &str) -> Result<T, String> {
    let resp = feedback_request(crate::network::get(url), API_REQUEST_TIMEOUT)
        .header("Authorization", &format!("Bearer {access}"))
        .header("Platform", "open_platform")
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;
    let body_text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read {url}: {e}"))?;
    let payload: Pan123Resp<T> = serde_json::from_str(&body_text)
        .map_err(|e| format!("decode {url}: {e}"))?;
    if payload.code != 0 {
        return Err(format!("api code={} msg={}", payload.code, payload.message));
    }
    payload.data.ok_or_else(|| "api returned no data".to_string())
}

// -- confirm + cleanup -----------------------------------------------------

fn confirm(id: &str, size: u64, sha256: &str) -> Result<(), String> {
    let body = serde_json::json!({ "id": id, "size": size, "sha256": sha256 });
    let url = format!("{}/feedback/confirm", super::NIYIEN_FEEDBACK_BASE);
    let resp = feedback_request(crate::network::post(&url), API_REQUEST_TIMEOUT)
        .header("Content-Type", "application/json")
        .send(serde_json::to_string(&body).map_err(|e| format!("encode: {e}"))?.as_str())
        .map_err(|e| format!("HTTP error: {e}"))?;
    let status = resp.status();
    if !(200..300).contains(&status.as_u16()) {
        return Err(format!("confirm status {status}"));
    }
    Ok(())
}

fn cleanup(inputs: &PackageInputs, options: &PackageOptions) {
    let mobile_consent = cfg!(any(target_os = "ios", target_os = "android"));
    // Truncate incidents.log (design D5).
    if let Some(p) = inputs.incidents_log.as_ref().filter(|_| !mobile_consent || options.include_incidents) {
        let _ = std::fs::OpenOptions::new()
            .write(true).truncate(true).open(p);
    }
    // Truncate OFX / Adobe plugin logs after a successful confirm
    // (feedback-include-plugin-logs §D4). Best-effort: a locked file is
    // tolerated, the rest of cleanup still runs.
    if let Some(p) = inputs.openfx_log.as_ref().filter(|_| !mobile_consent || options.include_current_log) {
        let _ = std::fs::OpenOptions::new()
            .write(true).truncate(true).open(p);
    }
    if let Some(p) = inputs.adobe_log.as_ref().filter(|_| !mobile_consent || options.include_current_log) {
        let _ = std::fs::OpenOptions::new()
            .write(true).truncate(true).open(p);
    }
    // Mark each crash zip uploaded.
    for crash in inputs.crash_zips.iter().filter(|_| !mobile_consent || options.include_crashes) {
        let mut marker = crash.clone();
        marker.set_extension("uploaded");
        let _ = std::fs::write(&marker, b"");
    }
}

// -- retry helper ----------------------------------------------------------

fn with_retry<T, F>(name: &str, mut f: F) -> Result<T, String>
where
    F: FnMut() -> Result<T, String>,
{
    let mut last_err = String::new();
    for attempt in 0..super::RETRY_ATTEMPTS {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = e;
                if attempt + 1 < super::RETRY_ATTEMPTS {
                    let backoff = super::BACKOFF_SECS.get(attempt as usize).copied().unwrap_or(8);
                    log::warn!(target: "feedback", "{name} attempt {} failed, retrying in {}s: {}",
                        attempt + 1, backoff, last_err);
                    std::thread::sleep(Duration::from_secs(backoff));
                }
            }
        }
    }
    Err(last_err)
}

// -- pending persistence ---------------------------------------------------

#[derive(Serialize, Deserialize)]
struct PendingDescriptor {
    id:           String,
    upload:       UploadContract,
    expires_at:   String,
    stage:        String, // "uploading" | "confirming"
    sha256:       String,
    size:         u64,
    created_at:   String,
}

fn persist_pending(id: &str, token: &TokenResponse, zip: &[u8], stage: &str, sha256: &str) -> std::io::Result<()> {
    let dir = match super::pending_feedback_dir() {
        Some(d) => d,
        None => return Err(std::io::Error::other("logger::log_dir() not initialized")),
    };
    std::fs::create_dir_all(&dir)?;
    let zip_path = dir.join(format!("{id}.zip"));
    let json_path = dir.join(format!("{id}.json"));

    if stage == "uploading" {
        std::fs::write(&zip_path, zip)?;
    }
    let descriptor = PendingDescriptor {
        id:         id.to_string(),
        upload:     token.upload.clone(),
        expires_at: token.expires_at.clone(),
        stage:      stage.to_string(),
        sha256:     sha256.to_string(),
        size:       zip.len() as u64,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let json_bytes = serde_json::to_vec_pretty(&descriptor)?;
    std::fs::write(&json_path, json_bytes)?;
    Ok(())
}

/// Startup hook. Walks the pending directory, retries each entry's stage,
/// and removes successfully recovered entries. Expired URLs are dropped.
pub fn retry_pending() {
    let dir = match super::pending_feedback_dir() {
        Some(d) => d,
        None => return,
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") { continue; }
        let mut json_bytes = Vec::new();
        if std::fs::File::open(&path).and_then(|mut f| f.read_to_end(&mut json_bytes)).is_err() {
            continue;
        }
        let desc: PendingDescriptor = match serde_json::from_slice(&json_bytes) {
            Ok(d) => d,
            Err(e) => {
                log::warn!(target: "feedback", "pending {:?} unreadable, dropping: {e}", path.display());
                drop_pending(&dir, &path);
                continue;
            }
        };
        if is_expired(&desc.expires_at) {
            log::info!(target: "feedback", "pending {} expired, dropping", desc.id);
            drop_pending(&dir, &path);
            continue;
        }
        let zip_path = dir.join(format!("{}.zip", desc.id));
        let result: Result<(), String> = match desc.stage.as_str() {
            "uploading" => {
                let zip_bytes = match std::fs::read(&zip_path) {
                    Ok(b) => b,
                    Err(e) => { log::warn!(target: "feedback", "pending {} zip missing: {e}", desc.id); continue; }
                };
                let token = TokenResponse {
                    id:         desc.id.clone(),
                    region:     "<retry>".into(),
                    upload:     desc.upload.clone(),
                    expires_at: desc.expires_at.clone(),
                };
                let (tx, _rx) = std::sync::mpsc::channel();
                let upload_res = match desc.upload.kind.as_str() {
                    "r2_presigned_put" => upload_r2(&token, &zip_bytes, &tx),
                    "pan123_multipart" => upload_pan123(&token, &zip_bytes, &tx),
                    other => Err(format!("unknown upload kind: {other}")),
                };
                match upload_res {
                    Ok(()) => with_retry("retry_confirm", || confirm(&desc.id, desc.size, &desc.sha256)),
                    Err(e) => Err(e),
                }
            }
            "confirming" => with_retry("retry_confirm", || confirm(&desc.id, desc.size, &desc.sha256)),
            other => Err(format!("unknown stage: {other}")),
        };
        match result {
            Ok(()) => {
                log::info!(target: "feedback", "pending {} recovered", desc.id);
                drop_pending(&dir, &path);
            }
            Err(e) => {
                log::warn!(target: "feedback", "pending {} retry failed: {e}", desc.id);
            }
        }
    }
}

fn is_expired(rfc3339: &str) -> bool {
    match chrono::DateTime::parse_from_rfc3339(rfc3339) {
        Ok(dt) => dt < chrono::Utc::now(),
        Err(_) => true, // unparseable = treat as expired
    }
}

fn drop_pending(dir: &std::path::Path, json_path: &std::path::Path) {
    let _ = std::fs::remove_file(json_path);
    if let Some(stem) = json_path.file_stem().and_then(|s| s.to_str()) {
        let _ = std::fs::remove_file(dir.join(format!("{stem}.zip")));
    }
}

// -- utils ----------------------------------------------------------------

fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(bytes);
    let d = h.finalize();
    let mut s = String::with_capacity(32);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    struct TestResponse {
        status: u16,
        header_delay: Duration,
        body_delay: Duration,
        body: &'static str,
        content_length: usize,
    }

    impl TestResponse {
        fn new(status: u16, body: &'static str) -> Self {
            Self { status, header_delay: Duration::ZERO, body_delay: Duration::ZERO, body, content_length: body.len() }
        }
    }

    fn upload_server(responses: Vec<TestResponse>) -> (String, std::thread::JoinHandle<Vec<(String, Vec<u8>)>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/upload", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut workers = Vec::new();
            for response in responses {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "expected upload attempt was not received");
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept failed: {error}"),
                    }
                };
                workers.push(std::thread::spawn(move || {
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    let header_end = loop {
                        let count = stream.read(&mut buffer).unwrap();
                        assert!(count > 0);
                        request.extend_from_slice(&buffer[..count]);
                        if let Some(end) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                            break end + 4;
                        }
                    };
                    let headers = String::from_utf8(request[..header_end].to_vec()).unwrap();
                    let size = headers.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                    }).unwrap_or(0);
                    while request.len() - header_end < size {
                        let count = stream.read(&mut buffer).unwrap();
                        assert!(count > 0);
                        request.extend_from_slice(&buffer[..count]);
                    }
                    std::thread::sleep(response.header_delay);
                    let _ = write!(stream, "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.status, response.content_length);
                    std::thread::sleep(response.body_delay);
                    let _ = stream.write_all(response.body.as_bytes());
                    (headers, request[header_end..].to_vec())
                }));
            }
            workers.into_iter().map(|worker| worker.join().unwrap()).collect()
        });
        (url, server)
    }

    fn r2_token(url: String) -> TokenResponse {
        TokenResponse {
            id: "test-report".into(), region: "global".into(), expires_at: "2099-01-01T00:00:00Z".into(),
            upload: UploadContract {
                method: "PUT".into(), kind: "r2_presigned_put".into(), url,
                headers: HashMap::from([("x-test-header".into(), "preserved".into())]),
                open_api_base: None, access_token: None, parent_file_id: None, parent_path: None, filename: None,
            },
        }
    }

    #[test]
    fn feedback_request_allows_delayed_headers_and_body() {
        for delay_body in [false, true] {
            let mut response = TestResponse::new(200, "ok");
            if delay_body { response.body_delay = Duration::from_millis(250); }
            else { response.header_delay = Duration::from_millis(250); }
            let (url, server) = upload_server(vec![response]);
            let request = crate::network::post(&url).config()
                .timeout_recv_response(Some(Duration::from_millis(100))).build();
            let mut response = feedback_request(request, Duration::from_secs(2)).send("report").unwrap();
            assert_eq!(response.body_mut().read_to_string().unwrap(), "ok");
            assert_eq!(server.join().unwrap()[0].1, b"report");
        }
    }

    #[test]
    fn feedback_request_bounds_total_transfer_time() {
        let mut response = TestResponse::new(200, "ok");
        response.body_delay = Duration::from_millis(400);
        let (url, server) = upload_server(vec![response]);
        let mut response = feedback_request(crate::network::post(&url), Duration::from_millis(150))
            .send("report").unwrap();
        let error = response.body_mut().read_to_string().unwrap_err();
        assert!(format!("{error:?}").contains("Timeout(Global)"));
        server.join().unwrap();
    }

    #[test]
    fn r2_retries_transient_failure_with_same_object_and_bytes() {
        let (url, server) = upload_server(vec![TestResponse::new(503, ""), TestResponse::new(200, "")]);
        let (tx, rx) = std::sync::mpsc::channel();
        upload_r2(&r2_token(url), b"zip bytes", &tx).unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], requests[1]);
        assert!(requests[0].0.starts_with("PUT /upload "));
        assert!(requests[0].0.to_ascii_lowercase().contains("x-test-header: preserved"));
        assert_eq!(requests[0].1, b"zip bytes");
        assert!(rx.try_iter().any(|state| matches!(state, FeedbackJobState::Uploading { pct: 100 })));
    }

    #[test]
    fn r2_does_not_retry_definitive_errors_or_report_success() {
        for status in [403, 404] {
            let (url, server) = upload_server(vec![TestResponse::new(status, "")]);
            let (tx, rx) = std::sync::mpsc::channel();
            assert!(upload_r2(&r2_token(url), b"zip bytes", &tx).is_err());
            assert_eq!(server.join().unwrap().len(), 1);
            assert!(!rx.try_iter().any(|state| matches!(state, FeedbackJobState::Uploading { pct: 100 })));
        }
    }

    #[test]
    fn r2_stops_after_upload_retry_budget() {
        let (url, server) = upload_server((0..super::super::RETRY_ATTEMPTS).map(|_| TestResponse::new(503, "")).collect());
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(upload_r2(&r2_token(url), b"zip bytes", &tx).is_err());
        assert_eq!(server.join().unwrap().len(), super::super::RETRY_ATTEMPTS as usize);
        assert!(!rx.try_iter().any(|state| matches!(state, FeedbackJobState::Uploading { pct: 100 })));
    }

    #[test]
    fn upload_retries_response_timeout_without_appending_partial_body() {
        let mut delayed = TestResponse::new(200, "bad");
        delayed.body_delay = Duration::from_millis(400);
        let (url, server) = upload_server(vec![delayed, TestResponse::new(200, "ok")]);
        let body = upload_request_with_retry("test upload", b"report", Duration::from_millis(150), || crate::network::put(&url)).unwrap();
        assert_eq!(body, "ok");
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], requests[1]);
    }

    #[test]
    fn pan123_slice_retries_short_response_with_identical_multipart_body() {
        let mut short = TestResponse::new(200, "bad");
        short.content_length = 6;
        let (url, server) = upload_server(vec![short, TestResponse::new(200, r#"{"code":0}"#)]);
        let checksum = md5_hex(b"zip bytes");
        upload_pan123_slice(&url, "test-token", "upload-1", 2, &checksum, b"zip bytes", "report.zip").unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], requests[1]);
        assert!(requests[0].0.starts_with("POST /upload "));
        assert!(requests[0].0.to_ascii_lowercase().contains("authorization: bearer test-token"));
        let body = String::from_utf8(requests[0].1.clone()).unwrap();
        for value in ["name=\"preuploadID\"\r\n\r\nupload-1", "name=\"sliceNo\"\r\n\r\n2", &checksum, "report.zip.part2", "zip bytes"] {
            assert!(body.contains(value), "missing multipart value: {value}");
        }
    }

    #[test]
    fn pan123_slice_preserves_api_error_without_retry() {
        let (url, server) = upload_server(vec![TestResponse::new(200, r#"{"code":401,"message":"invalid token"}"#)]);
        let error = upload_pan123_slice(&url, "test-token", "upload-1", 1, "checksum", b"zip", "report.zip").unwrap_err();
        assert!(error.contains("api code=401"));
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[test]
    fn md5_roundtrip() {
        // Known MD5("hello") = 5d41402abc4b2a76b9719d911017c592
        assert_eq!(md5_hex(b"hello"), "5d41402abc4b2a76b9719d911017c592");
    }

    #[test]
    fn retry_does_eventual_success() {
        let mut count = 0;
        let result: Result<i32, String> = with_retry("test", || {
            count += 1;
            if count < 3 { Err("transient".into()) } else { Ok(42) }
        });
        assert_eq!(result, Ok(42));
        assert_eq!(count, 3);
    }

    #[test]
    fn retry_gives_up_after_attempts() {
        // Note: this sleeps RETRY_ATTEMPTS * BACKOFF_SECS during real run
        // (~7s); use immediate fail wrapper that doesn't sleep on last attempt.
        let mut count = 0;
        let result: Result<i32, String> = with_retry("test", || {
            count += 1;
            Err("always".into())
        });
        assert!(result.is_err());
        assert_eq!(count as u32, super::super::RETRY_ATTEMPTS);
    }

    #[test]
    fn is_expired_basic() {
        assert!(is_expired("2020-01-01T00:00:00Z"));
        assert!(!is_expired("2099-01-01T00:00:00Z"));
        assert!(is_expired("not a date"));
    }
}
