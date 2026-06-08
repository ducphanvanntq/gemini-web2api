//! Multimodal: Scotty resumable upload for Gemini image input.
//! Mirrors gemini_web2api/multimodal.py.
use crate::config::config;
use crate::gemini::{load_cookie, log, make_sapisidhash, HTTP};
use crate::tools::ImageItem;
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue};
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct PageTokenCache {
    tokens: std::collections::HashMap<String, String>,
    ts: Option<Instant>,
}

static PAGE_TOKENS: Lazy<Mutex<PageTokenCache>> =
    Lazy::new(|| Mutex::new(PageTokenCache { tokens: Default::default(), ts: None }));

static RE_PUSH: Lazy<Regex> = Lazy::new(|| Regex::new(r#""qKIAYe":"([^"]+)""#).unwrap());
static RE_PCTX: Lazy<Regex> = Lazy::new(|| Regex::new(r#""Ylro7b":"([^"]+)""#).unwrap());
static RE_AT: Lazy<Regex> = Lazy::new(|| Regex::new(r#""thykhd":"([^"]+)""#).unwrap());

async fn get_page_tokens() -> std::collections::HashMap<String, String> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "User-Agent",
        HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"),
    );
    let (cookie_str, _) = load_cookie();
    if !cookie_str.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&cookie_str) {
            headers.insert("Cookie", v);
        }
    }
    let mut tokens = std::collections::HashMap::new();
    match HTTP.get("https://gemini.google.com/app").headers(headers).send().await {
        Ok(resp) => match resp.text().await {
            Ok(html) => {
                if let Some(c) = RE_PUSH.captures(&html) {
                    tokens.insert("push_id".to_string(), c[1].to_string());
                }
                if let Some(c) = RE_PCTX.captures(&html) {
                    tokens.insert("pctx".to_string(), c[1].to_string());
                }
                if let Some(c) = RE_AT.captures(&html) {
                    tokens.insert("at".to_string(), c[1].to_string());
                }
            }
            Err(e) => log(&format!("Page token fetch failed: {e}")),
        },
        Err(e) => log(&format!("Page token fetch failed: {e}")),
    }
    tokens
}

async fn cached_page_tokens() -> std::collections::HashMap<String, String> {
    {
        let cache = PAGE_TOKENS.lock().unwrap();
        if let Some(ts) = cache.ts {
            if ts.elapsed() < Duration::from_secs(600) {
                return cache.tokens.clone();
            }
        }
    }
    let tokens = get_page_tokens().await;
    let mut cache = PAGE_TOKENS.lock().unwrap();
    cache.tokens = tokens.clone();
    cache.ts = Some(Instant::now());
    tokens
}

/// Upload an image via Scotty resumable upload. Returns the file reference path.
pub async fn upload_image(
    image_bytes: &[u8],
    filename: &str,
    mime_type: &str,
) -> Result<String, String> {
    let tokens = cached_page_tokens().await;
    let push_id = tokens.get("push_id").cloned().unwrap_or_else(|| "feeds/mcudyrk2a4khkz".to_string());
    let pctx = tokens.get("pctx").cloned().unwrap_or_else(|| "CgcSBWjK7pYx".to_string());

    let (cookie_str, sapisid) = load_cookie();

    // Step 1: initiate resumable upload
    let mut start_headers = HeaderMap::new();
    let set = |h: &mut HeaderMap, k: &'static str, v: &str| {
        if let Ok(val) = HeaderValue::from_str(v) {
            h.insert(k, val);
        }
    };
    set(&mut start_headers, "Push-ID", &push_id);
    start_headers.insert("X-Tenant-Id", HeaderValue::from_static("bard-storage"));
    set(&mut start_headers, "X-Client-Pctx", &pctx);
    set(&mut start_headers, "X-Goog-Upload-Header-Content-Length", &image_bytes.len().to_string());
    set(&mut start_headers, "X-Goog-Upload-Header-Content-Type", mime_type);
    start_headers.insert("X-Goog-Upload-Protocol", HeaderValue::from_static("resumable"));
    start_headers.insert("X-Goog-Upload-Command", HeaderValue::from_static("start"));
    start_headers.insert(
        "Content-Type",
        HeaderValue::from_static("application/x-www-form-urlencoded;charset=utf-8"),
    );
    start_headers.insert(
        "User-Agent",
        HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"),
    );
    if !cookie_str.is_empty() {
        set(&mut start_headers, "Cookie", &cookie_str);
    }
    if let Some(s) = &sapisid {
        set(&mut start_headers, "Authorization", &make_sapisidhash(s));
    }

    let start_resp = HTTP
        .post("https://content-push.googleapis.com/upload/")
        .headers(start_headers)
        .body(Vec::<u8>::new())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let upload_url = start_resp
        .headers()
        .get("X-Goog-Upload-URL")
        .or_else(|| start_resp.headers().get("x-goog-upload-url"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| "No upload URL in response headers".to_string())?;

    log(&format!("Upload session started: {}...", &upload_url[..upload_url.len().min(80)]));

    // Step 2: upload file data + finalize
    let mut upload_headers = HeaderMap::new();
    upload_headers.insert("X-Goog-Upload-Command", HeaderValue::from_static("upload, finalize"));
    upload_headers.insert("X-Goog-Upload-Offset", HeaderValue::from_static("0"));
    upload_headers.insert("Content-Type", HeaderValue::from_static("application/octet-stream"));
    upload_headers.insert(
        "User-Agent",
        HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"),
    );

    let up_resp = HTTP
        .post(&upload_url)
        .headers(upload_headers)
        .body(image_bytes.to_vec())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let file_ref = up_resp.text().await.map_err(|e| e.to_string())?.trim().to_string();
    if file_ref.is_empty() || !file_ref.starts_with('/') {
        return Err(format!("Invalid file reference: {}", &file_ref[..file_ref.len().min(100)]));
    }
    log(&format!("Image uploaded: {filename} -> {}...", &file_ref[..file_ref.len().min(50)]));
    Ok(file_ref)
}

/// Upload a list of decoded images and return their file references.
/// Returns None if the list is empty or all uploads fail.
pub async fn upload_images(images: &[ImageItem]) -> Option<Vec<String>> {
    if images.is_empty() {
        return None;
    }
    let _ = config(); // ensure config is initialized
    let mut refs = Vec::new();
    for (data, mime) in images {
        if data.is_empty() {
            continue;
        }
        let m = if mime.is_empty() { "image/png" } else { mime.as_str() };
        match upload_image(data, "image.png", m).await {
            Ok(r) => refs.push(r),
            Err(e) => log(&format!("Image upload failed: {e}")),
        }
    }
    if refs.is_empty() {
        None
    } else {
        Some(refs)
    }
}
