//! Gemini StreamGenerate protocol implementation. Mirrors gemini_web2api/gemini.py.
use crate::config::config;
use futures::Stream;
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn log(msg: &str) {
    if config().log_requests {
        let now = chrono_like_time();
        eprintln!("[{now}] {msg}");
    }
}

/// Simple HH:MM:SS timestamp without pulling in chrono.
fn chrono_like_time() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    format!("{h:02}:{m:02}:{s:02}")
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

// ─── HTTP client ───────────────────────────────────────────────────────────

pub static HTTP: Lazy<reqwest::Client> = Lazy::new(|| {
    let cfg = config();
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(cfg.request_timeout_sec));
    if let Some(proxy) = &cfg.proxy {
        if let Ok(p) = reqwest::Proxy::all(proxy) {
            builder = builder.proxy(p);
        }
    }
    builder.build().expect("failed to build http client")
});

// ─── Cookie loading with mtime cache ─────────────────────────────────────────

struct CookieCache {
    cookie: String,
    sapisid: Option<String>,
    mtime: Option<std::time::SystemTime>,
}

static COOKIE_CACHE: Lazy<Mutex<CookieCache>> =
    Lazy::new(|| Mutex::new(CookieCache { cookie: String::new(), sapisid: None, mtime: None }));

/// Load cookie from file with mtime-based caching. Returns (cookie_str, sapisid).
pub fn load_cookie() -> (String, Option<String>) {
    let cookie_file = match &config().cookie_file {
        Some(f) if std::path::Path::new(f).exists() => f.clone(),
        _ => return (String::new(), None),
    };

    let mtime = std::fs::metadata(&cookie_file).ok().and_then(|m| m.modified().ok());
    {
        let cache = COOKIE_CACHE.lock().unwrap();
        if cache.mtime == mtime && !cache.cookie.is_empty() {
            return (cache.cookie.clone(), cache.sapisid.clone());
        }
    }

    match std::fs::read_to_string(&cookie_file) {
        Ok(raw) => {
            let content = raw.trim().to_string();
            let (cookie_str, sapisid) = if content.starts_with('{') {
                match serde_json::from_str::<Value>(&content) {
                    Ok(data) => (
                        data.get("cookie").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        data.get("sapisid").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    ),
                    Err(_) => (content.clone(), None),
                }
            } else {
                let mut sapisid = None;
                for pair in content.split("; ") {
                    if let Some((k, v)) = pair.split_once('=') {
                        if k == "SAPISID" {
                            sapisid = Some(v.to_string());
                        }
                    }
                }
                (content.clone(), sapisid)
            };
            let sapisid = sapisid.filter(|s| !s.is_empty());
            let mut cache = COOKIE_CACHE.lock().unwrap();
            cache.cookie = cookie_str.clone();
            cache.sapisid = sapisid.clone();
            cache.mtime = mtime;
            (cookie_str, sapisid)
        }
        Err(e) => {
            log(&format!("Cookie load error: {e}"));
            let cache = COOKIE_CACHE.lock().unwrap();
            (cache.cookie.clone(), cache.sapisid.clone())
        }
    }
}

pub fn make_sapisidhash(sapisid: &str) -> String {
    let ts = now_secs();
    let mut hasher = Sha1::new();
    hasher.update(format!("{ts} {sapisid} https://gemini.google.com").as_bytes());
    let h = hex::encode(hasher.finalize());
    format!("SAPISIDHASH {ts}_{h}")
}

fn account_prefix() -> String {
    match config().auth_user_str() {
        Some(u) => format!("/u/{u}"),
        None => String::new(),
    }
}

pub fn build_headers() -> HeaderMap {
    let prefix = account_prefix();
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type", HeaderValue::from_static("application/x-www-form-urlencoded"));
    headers.insert("Origin", HeaderValue::from_static("https://gemini.google.com"));
    if let Ok(v) = HeaderValue::from_str(&format!("https://gemini.google.com{prefix}/app")) {
        headers.insert("Referer", v);
    }
    headers.insert("X-Same-Domain", HeaderValue::from_static("1"));
    headers.insert(
        "User-Agent",
        HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"),
    );
    if !prefix.is_empty() {
        if let Some(u) = config().auth_user_str() {
            if let Ok(v) = HeaderValue::from_str(&u) {
                headers.insert("X-Goog-AuthUser", v);
            }
        }
    }
    let (cookie_str, sapisid) = load_cookie();
    if !cookie_str.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&cookie_str) {
            headers.insert("Cookie", v);
        }
    }
    if let Some(s) = sapisid {
        if let Ok(v) = HeaderValue::from_str(&make_sapisidhash(&s)) {
            headers.insert(HeaderName::from_static("authorization"), v);
        }
    }
    headers
}

/// Build the urlencoded `f.req` body for a StreamGenerate request.
pub fn build_payload(
    prompt: &str,
    model_id: i64,
    think_mode: i64,
    file_refs: Option<&[String]>,
    extra: Option<&[(usize, i64)]>,
) -> String {
    let mut inner: Vec<Value> = vec![Value::Null; 102];

    match file_refs {
        Some(refs) if !refs.is_empty() => {
            let r: Vec<Value> = refs.iter().map(|x| json!([null, null, x])).collect();
            inner[0] = json!([prompt, 0, null, r, null, null, 0]);
        }
        _ => {
            inner[0] = json!([prompt, 0, null, null, null, null, 0]);
        }
    }
    inner[1] = json!(["en"]);
    inner[2] = json!(["", "", "", null, null, null, null, null, null, ""]);
    inner[6] = json!([0]);
    inner[7] = json!(1);
    inner[10] = json!(1);
    inner[11] = json!(0);
    inner[17] = json!([[think_mode]]);
    inner[18] = json!(0);
    inner[27] = json!(1);
    inner[30] = json!([4]);
    inner[41] = json!([2]);
    inner[53] = json!(0);
    inner[59] = json!(uuid::Uuid::new_v4().to_string());
    inner[61] = json!([]);
    inner[68] = json!(1);
    inner[79] = json!(model_id);
    if let Some(ex) = extra {
        for (k, v) in ex {
            inner[*k] = json!(v);
        }
    }

    let inner_str = serde_json::to_string(&Value::Array(inner)).unwrap();
    let outer_str = serde_json::to_string(&json!([null, inner_str])).unwrap();

    let mut ser = form_urlencoded::Serializer::new(String::new());
    ser.append_pair("f.req", &outer_str);
    if let Some(tok) = &config().xsrf_token {
        ser.append_pair("at", tok);
    }
    ser.finish()
}

pub fn get_url() -> String {
    let reqid = now_secs() % 1_000_000;
    let prefix = account_prefix();
    let cfg = config();
    format!(
        "https://gemini.google.com{prefix}/_/BardChatUi/data/\
assistant.lamda.BardFrontendService/StreamGenerate?bl={}&hl=en&_reqid={reqid}&rt=c",
        cfg.gemini_bl
    )
}

// ─── Response parsing ────────────────────────────────────────────────────────

static RE_CODE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)```(?:python|javascript|text)\?code_(?:reference|stdout)&code_event_index=\d+\n.*?```\n?").unwrap()
});
static RE_CARD: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"http://googleusercontent\.com/card_content/\d+\n?").unwrap());

pub fn clean_text(text: &str) -> String {
    let a = RE_CODE.replace_all(text, "");
    let b = RE_CARD.replace_all(&a, "");
    b.trim().to_string()
}

/// Parse a single wrb.fr line and return the list of text strings found.
fn extract_texts_from_line(line: &str) -> Vec<String> {
    if !line.contains("\"wrb.fr\"") || line.len() < 200 {
        return vec![];
    }
    let arr: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let inner_str = match arr.get(0).and_then(|a| a.get(2)).and_then(|s| s.as_str()) {
        Some(s) if s.len() >= 50 => s,
        _ => return vec![],
    };
    let inner: Value = match serde_json::from_str(inner_str) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let parts = match inner.get(4).and_then(|v| v.as_array()) {
        Some(p) if !p.is_empty() => p,
        _ => return vec![],
    };
    let mut texts = Vec::new();
    for part in parts {
        if let Some(pa) = part.as_array() {
            if pa.len() > 1 {
                if let Some(list) = pa[1].as_array() {
                    for t in list {
                        if let Some(s) = t.as_str() {
                            if !s.is_empty() {
                                texts.push(s.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    texts
}

/// Parse a full response body to get the final (longest) text.
pub fn extract_response_text(raw: &str) -> String {
    let mut last_text = String::new();
    for line in raw.split('\n') {
        for t in extract_texts_from_line(line) {
            if t.len() > last_text.len() {
                last_text = t;
            }
        }
    }
    clean_text(&last_text)
}

// ─── Generation ──────────────────────────────────────────────────────────────

/// Non-streaming generation with retry. Returns the final text.
pub async fn generate(
    prompt: &str,
    model_id: i64,
    think_mode: i64,
    file_refs: Option<&[String]>,
    extra: Option<&[(usize, i64)]>,
) -> Result<String, String> {
    let body = build_payload(prompt, model_id, think_mode, file_refs, extra);
    let url = get_url();
    let cfg = config();

    let mut last_err = String::from("unknown error");
    for attempt in 0..cfg.retry_attempts {
        let headers = build_headers();
        match HTTP.post(&url).headers(headers).body(body.clone()).send().await {
            Ok(resp) => match resp.text().await {
                Ok(raw) => return Ok(extract_response_text(&raw)),
                Err(e) => last_err = e.to_string(),
            },
            Err(e) => last_err = e.to_string(),
        }
        if attempt < cfg.retry_attempts - 1 {
            log(&format!("Retry {}/{}: {last_err}", attempt + 1, cfg.retry_attempts));
            tokio::time::sleep(std::time::Duration::from_secs(cfg.retry_delay_sec)).await;
        }
    }
    Err(last_err)
}

/// Streaming generation. Yields incremental text deltas.
pub fn generate_stream(
    prompt: String,
    model_id: i64,
    think_mode: i64,
    file_refs: Option<Vec<String>>,
    extra: Option<Vec<(usize, i64)>>,
) -> impl Stream<Item = String> {
    use futures::StreamExt;

    async_stream::stream! {
        let body = build_payload(prompt.as_str(), model_id, think_mode, file_refs.as_deref(), extra.as_deref());
        let url = get_url();
        let cfg = config();

        for attempt in 0..cfg.retry_attempts {
            let headers = build_headers();
            let send_result = HTTP.post(&url).headers(headers).body(body.clone()).send().await;
            match send_result {
                Ok(resp) => {
                    let mut prev_text = String::new();
                    let mut buf = String::new();
                    let mut byte_stream = resp.bytes_stream();
                    let mut stream_err = false;
                    while let Some(chunk) = byte_stream.next().await {
                        match chunk {
                            Ok(bytes) => {
                                buf.push_str(&String::from_utf8_lossy(&bytes));
                                while let Some(pos) = buf.find('\n') {
                                    let line: String = buf[..pos].to_string();
                                    buf = buf[pos + 1..].to_string();
                                    for t in extract_texts_from_line(&line) {
                                        if t.len() > prev_text.len() {
                                            let delta = clean_text(&t[prev_text.len()..]);
                                            if !delta.is_empty() {
                                                yield delta;
                                            }
                                            prev_text = t;
                                        }
                                    }
                                }
                            }
                            Err(_) => { stream_err = true; break; }
                        }
                    }
                    if !stream_err {
                        return;
                    }
                }
                Err(e) => {
                    if attempt < cfg.retry_attempts - 1 {
                        log(&format!("Stream retry {}/{}: {e}", attempt + 1, cfg.retry_attempts));
                        tokio::time::sleep(std::time::Duration::from_secs(cfg.retry_delay_sec)).await;
                    }
                }
            }
        }
    }
}
