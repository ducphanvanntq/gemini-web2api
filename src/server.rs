//! HTTP server: OpenAI-compatible API endpoints. Mirrors gemini_web2api/server.py.
use crate::config::config;
use crate::gemini::{generate, generate_stream, log};
use crate::models::{resolve_model, Resolved, MODELS};
use crate::multimodal::upload_images;
use crate::tools::{
    google_contents_to_prompt, messages_to_prompt, parse_google_function_calls, parse_tool_calls,
    ImageItem,
};
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};

pub const VERSION: &str = "1.1.0";

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn tok(s: &str) -> u64 {
    (s.chars().count() / 4) as u64
}

fn new_id(prefix: &str, n: usize) -> String {
    format!("{prefix}{}", &uuid::Uuid::new_v4().simple().to_string()[..n])
}

fn json_response(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

fn err_response(status: StatusCode, msg: &str) -> Response {
    json_response(status, json!({"error": {"message": msg}}))
}

fn sse_response<S>(stream: S) -> Response
where
    S: futures::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
{
    Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(Body::from_stream(stream))
        .unwrap()
}

fn authorized(headers: &HeaderMap) -> bool {
    let keys = &config().api_keys;
    if keys.is_empty() {
        return true;
    }
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
    let key = if let Some(rest) = auth.strip_prefix("Bearer ") {
        rest.to_string()
    } else {
        headers.get("x-api-key").and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
    };
    keys.iter().any(|k| k == &key)
}

fn parse_body(body: &Bytes) -> Option<Value> {
    serde_json::from_slice::<Value>(body).ok()
}

async fn refs_for(images: &[ImageItem]) -> Option<Vec<String>> {
    upload_images(images).await
}

// ─── GET handlers ────────────────────────────────────────────────────────────

pub async fn get_root() -> Response {
    json_response(
        StatusCode::OK,
        json!({
            "status": "ok",
            "version": VERSION,
            "models": MODELS.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        }),
    )
}

pub async fn get_models(headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return err_response(StatusCode::UNAUTHORIZED, "invalid api key");
    }
    let data: Vec<Value> = MODELS
        .iter()
        .map(|(n, c)| {
            json!({"id": n, "object": "model", "created": 1700000000, "owned_by": "google", "description": c.desc})
        })
        .collect();
    json_response(StatusCode::OK, json!({"object": "list", "data": data}))
}

pub async fn get_v1beta_models() -> Response {
    let models: Vec<Value> = MODELS
        .iter()
        .map(|(n, c)| {
            json!({
                "name": format!("models/{n}"),
                "displayName": n,
                "description": c.desc,
                "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
            })
        })
        .collect();
    json_response(StatusCode::OK, json!({"models": models}))
}

// ─── /v1/chat/completions ─────────────────────────────────────────────────────

pub async fn post_chat(headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&headers) {
        return err_response(StatusCode::UNAUTHORIZED, "invalid api key");
    }
    let req = match parse_body(&body) {
        Some(v) => v,
        None => return err_response(StatusCode::BAD_REQUEST, "invalid JSON"),
    };

    let model_req = req.get("model").and_then(|v| v.as_str()).unwrap_or(&config().default_model).to_string();
    let Resolved { name: model_name, mode: model_id, think: think_mode, extra } =
        match resolve_model(&model_req, &config().default_model) {
            Ok(r) => r,
            Err(e) => return err_response(StatusCode::BAD_REQUEST, &e),
        };

    let tools = req.get("tools").and_then(|v| v.as_array()).cloned();
    let tool_choice = req.get("tool_choice").cloned().unwrap_or(json!("auto"));
    let messages = req.get("messages").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let (prompt, images) = messages_to_prompt(&messages, tools.as_ref(), &tool_choice);
    if prompt.trim().is_empty() {
        return err_response(StatusCode::BAD_REQUEST, "empty prompt");
    }

    let stream = req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    let cid = new_id("chatcmpl-", 12);
    let has_tools = tools.as_ref().map_or(false, |t| !t.is_empty());
    let tc_none = tool_choice == "none";

    let file_refs = refs_for(&images).await;

    // Streaming, no tool calling: stream text deltas.
    if stream && (!has_tools || tc_none) {
        let cid2 = cid.clone();
        let model2 = model_name.clone();
        let st = async_stream::stream! {
            let inner = generate_stream(prompt, model_id, think_mode, file_refs, extra);
            futures::pin_mut!(inner);
            while let Some(delta) = inner.next().await {
                let chunk = json!({
                    "id": cid2, "object": "chat.completion.chunk", "created": now(),
                    "model": model2,
                    "choices": [{"index": 0, "delta": {"content": delta}, "finish_reason": Value::Null}],
                });
                yield Ok::<_, std::io::Error>(Bytes::from(format!("data: {}\n\n", chunk)));
            }
            let end = json!({
                "id": cid2, "object": "chat.completion.chunk", "created": now(),
                "model": model2,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            });
            yield Ok(Bytes::from(format!("data: {}\n\n", end)));
            yield Ok(Bytes::from("data: [DONE]\n\n".to_string()));
        };
        return sse_response(st);
    }

    // Non-streaming (or tool-calling path).
    let text = match generate(&prompt, model_id, think_mode, file_refs.as_deref(), extra.as_deref()).await {
        Ok(t) => t,
        Err(e) => return err_response(StatusCode::BAD_GATEWAY, &format!("upstream error: {e}")),
    };

    let mut tool_calls: Vec<Value> = Vec::new();
    let mut final_text = text.clone();
    if has_tools && !text.is_empty() && !tc_none {
        let (clean, tcs) = parse_tool_calls(&text);
        final_text = clean;
        tool_calls = tcs;
    }

    let content_val = if final_text.is_empty() { Value::Null } else { json!(final_text) };
    let mut msg = json!({"role": "assistant", "content": content_val});
    if !tool_calls.is_empty() {
        msg["tool_calls"] = json!(tool_calls);
    }
    let finish = if !tool_calls.is_empty() { "tool_calls" } else { "stop" };

    if stream {
        let cid2 = cid.clone();
        let model2 = model_name.clone();
        let chunk = json!({
            "id": cid2, "object": "chat.completion.chunk", "created": now(),
            "model": model2,
            "choices": [{"index": 0, "delta": msg, "finish_reason": finish}],
        });
        let st = async_stream::stream! {
            yield Ok::<_, std::io::Error>(Bytes::from(format!("data: {}\n\n", chunk)));
            yield Ok(Bytes::from("data: [DONE]\n\n".to_string()));
        };
        return sse_response(st);
    }

    let pt = tok(&prompt);
    let ct = tok(&final_text);
    json_response(
        StatusCode::OK,
        json!({
            "id": cid, "object": "chat.completion", "created": now(),
            "model": model_name,
            "choices": [{"index": 0, "message": msg, "finish_reason": finish}],
            "usage": {"prompt_tokens": pt, "completion_tokens": ct, "total_tokens": pt + ct},
        }),
    )
}

// ─── /v1/responses (Codex CLI) ────────────────────────────────────────────────

pub async fn post_responses(headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&headers) {
        return err_response(StatusCode::UNAUTHORIZED, "invalid api key");
    }
    let req = match parse_body(&body) {
        Some(v) => v,
        None => return err_response(StatusCode::BAD_REQUEST, "invalid JSON"),
    };

    let model_req = req.get("model").and_then(|v| v.as_str()).unwrap_or(&config().default_model).to_string();
    let Resolved { name: model_name, mode: model_id, think: think_mode, extra } =
        match resolve_model(&model_req, &config().default_model) {
            Ok(r) => r,
            Err(e) => return err_response(StatusCode::BAD_REQUEST, &e),
        };

    // Build OpenAI-style messages from the Responses `input`.
    let mut messages: Vec<Value> = Vec::new();
    if let Some(instr) = req.get("instructions").and_then(|v| v.as_str()) {
        messages.push(json!({"role": "system", "content": instr}));
    }
    let input = req.get("input").cloned().unwrap_or(Value::Null);
    match input {
        Value::String(s) => messages.push(json!({"role": "user", "content": s})),
        Value::Array(items) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    messages.push(json!({"role": "user", "content": s}));
                    continue;
                }
                if !item.is_object() {
                    continue;
                }
                let itype = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let irole = item.get("role").and_then(|v| v.as_str()).unwrap_or("");
                if itype == "function_call_output" {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": item.get("call_id").and_then(|v| v.as_str()).unwrap_or(""),
                        "name": item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "content": item.get("output").and_then(|v| v.as_str()).unwrap_or(""),
                    }));
                } else if irole == "assistant" || (itype == "message" && irole == "assistant") {
                    let cp = item.get("content").cloned().unwrap_or(Value::Null);
                    let mut text_acc = String::new();
                    let mut tc_list: Vec<Value> = Vec::new();
                    match cp {
                        Value::Array(arr) => {
                            for c in arr {
                                let ct = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                if ct == "output_text" {
                                    text_acc.push_str(c.get("text").and_then(|v| v.as_str()).unwrap_or(""));
                                } else if ct == "function_call" {
                                    tc_list.push(c);
                                }
                            }
                        }
                        Value::String(s) => text_acc = s,
                        _ => {}
                    }
                    let mut m = json!({"role": "assistant", "content": if text_acc.is_empty() { Value::Null } else { json!(text_acc) }});
                    if !tc_list.is_empty() {
                        let calls: Vec<Value> = tc_list
                            .iter()
                            .enumerate()
                            .map(|(i, tc)| {
                                json!({
                                    "id": tc.get("call_id").and_then(|v| v.as_str()).map(String::from).unwrap_or(format!("call_{i}")),
                                    "type": "function",
                                    "function": {
                                        "name": tc.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                                        "arguments": tc.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}"),
                                    }
                                })
                            })
                            .collect();
                        m["tool_calls"] = json!(calls);
                    }
                    messages.push(m);
                } else {
                    let role = if irole.is_empty() { "user" } else { irole };
                    let content = item.get("content").cloned().unwrap_or(json!(""));
                    let content_str = match content {
                        Value::Array(arr) => arr
                            .iter()
                            .filter(|c| {
                                matches!(c.get("type").and_then(|v| v.as_str()), Some("text") | Some("input_text"))
                            })
                            .filter_map(|c| c.get("text").and_then(|v| v.as_str()))
                            .collect::<Vec<_>>()
                            .join(" "),
                        Value::String(s) => s,
                        other => other.to_string(),
                    };
                    messages.push(json!({"role": role, "content": content_str}));
                }
            }
        }
        _ => {}
    }

    // Normalize Responses-style tools to OpenAI chat tools.
    let tools: Option<Vec<Value>> = req.get("tools").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .map(|t| {
                if t.get("type").and_then(|v| v.as_str()) == Some("function") && t.get("function").is_none() {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                            "description": t.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                            "parameters": t.get("parameters").cloned().unwrap_or(json!({})),
                        }
                    })
                } else {
                    t.clone()
                }
            })
            .collect()
    });

    let tool_choice = req.get("tool_choice").cloned().unwrap_or(json!("auto"));
    let (prompt, images) = messages_to_prompt(&messages, tools.as_ref(), &tool_choice);
    if prompt.trim().is_empty() {
        return err_response(StatusCode::BAD_REQUEST, "empty input");
    }

    let file_refs = refs_for(&images).await;
    let text = match generate(&prompt, model_id, think_mode, file_refs.as_deref(), extra.as_deref()).await {
        Ok(t) => t,
        Err(e) => return err_response(StatusCode::BAD_GATEWAY, &format!("upstream error: {e}")),
    };

    let has_tools = tools.as_ref().map_or(false, |t| !t.is_empty());
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut final_text = text.clone();
    if has_tools && !text.is_empty() && tool_choice != "none" {
        let (clean, tcs) = parse_tool_calls(&text);
        final_text = clean;
        tool_calls = tcs;
    }

    let rid = new_id("resp_", 16);
    let mid = new_id("msg_", 12);
    let mut output: Vec<Value> = Vec::new();
    for tc in &tool_calls {
        output.push(json!({
            "type": "function_call",
            "id": tc["id"], "call_id": tc["id"],
            "name": tc["function"]["name"], "arguments": tc["function"]["arguments"],
            "status": "completed",
        }));
    }
    if !final_text.is_empty() || tool_calls.is_empty() {
        output.push(json!({
            "type": "message", "id": mid, "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": final_text, "annotations": []}],
        }));
    }

    let pt = tok(&prompt);
    let ot = tok(&final_text);
    let stream = req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    if stream {
        let rid2 = rid.clone();
        let model2 = model_name.clone();
        let output2 = output.clone();
        let st = async_stream::stream! {
            let created = json!({"type": "response.created", "response": {"id": rid2, "object": "response", "status": "in_progress", "model": model2, "output": []}});
            yield Ok::<_, std::io::Error>(Bytes::from(format!("event: response.created\ndata: {}\n\n", created)));
            for item in &output2 {
                match item.get("type").and_then(|v| v.as_str()) {
                    Some("function_call") => {
                        let ev = json!({"type": "response.function_call_arguments.done", "item_id": item["id"], "call_id": item["call_id"], "name": item["name"], "arguments": item["arguments"]});
                        yield Ok(Bytes::from(format!("event: response.function_call_arguments.done\ndata: {}\n\n", ev)));
                    }
                    Some("message") => {
                        if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                            for (ci, cp) in content.iter().enumerate() {
                                let ev = json!({"type": "response.output_text.done", "item_id": item["id"], "content_index": ci, "text": cp["text"]});
                                yield Ok(Bytes::from(format!("event: response.output_text.done\ndata: {}\n\n", ev)));
                            }
                        }
                    }
                    _ => {}
                }
            }
            let resp_obj = json!({"id": rid2, "object": "response", "status": "completed", "model": model2, "output": output2,
                "usage": {"input_tokens": pt, "output_tokens": ot, "total_tokens": pt + ot}});
            let done = json!({"type": "response.completed", "response": resp_obj});
            yield Ok(Bytes::from(format!("event: response.completed\ndata: {}\n\n", done)));
        };
        return sse_response(st);
    }

    json_response(
        StatusCode::OK,
        json!({
            "id": rid, "object": "response", "created_at": now(), "status": "completed",
            "model": model_name, "output": output,
            "usage": {"input_tokens": pt, "output_tokens": ot, "total_tokens": pt + ot},
        }),
    )
}

// ─── /v1beta/models (Google Gemini CLI) ───────────────────────────────────────

static RE_MODEL_PATH: Lazy<Regex> = Lazy::new(|| Regex::new(r"/v1beta/models/([^:?]+)").unwrap());

async fn handle_google_generate(path: &str, body: Bytes, stream: bool) -> Response {
    let req = match parse_body(&body) {
        Some(v) => v,
        None => return err_response(StatusCode::BAD_REQUEST, "invalid JSON"),
    };
    let model_req = RE_MODEL_PATH
        .captures(path)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| config().default_model.clone());
    let Resolved { name: model_name, mode: model_id, think: think_mode, extra } =
        match resolve_model(&model_req, &config().default_model) {
            Ok(r) => r,
            Err(e) => return err_response(StatusCode::BAD_REQUEST, &e),
        };

    let fc_mode = req
        .get("toolConfig")
        .and_then(|v| v.get("functionCallingConfig"))
        .and_then(|v| v.get("mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("AUTO");
    let has_tools = req.get("tools").map_or(false, |t| !t.is_null()) && fc_mode != "NONE";

    let (prompt, images) = google_contents_to_prompt(&req);
    if prompt.trim().is_empty() {
        return err_response(StatusCode::BAD_REQUEST, "empty content");
    }

    let file_refs = refs_for(&images).await;
    log(&format!("Google API: model={model_name} stream={stream} tools={has_tools} prompt_len={}", prompt.chars().count()));

    if stream && !has_tools {
        let model2 = model_name.clone();
        let prompt_len = prompt.chars().count() as u64;
        let st = async_stream::stream! {
            let mut full_text = String::new();
            let inner = generate_stream(prompt, model_id, think_mode, file_refs, extra);
            futures::pin_mut!(inner);
            while let Some(delta) = inner.next().await {
                if delta.is_empty() { continue; }
                full_text.push_str(&delta);
                let chunk = json!({
                    "candidates": [{"content": {"parts": [{"text": delta}], "role": "model"}, "index": 0}],
                    "modelVersion": model2,
                });
                yield Ok::<_, std::io::Error>(Bytes::from(format!("data: {}\n\n", chunk)));
            }
            let ct = (full_text.chars().count() / 4) as u64;
            let final_chunk = json!({
                "candidates": [{"finishReason": "STOP", "index": 0}],
                "usageMetadata": {"promptTokenCount": prompt_len / 4, "candidatesTokenCount": ct, "totalTokenCount": prompt_len / 4 + ct},
                "modelVersion": model2,
            });
            yield Ok(Bytes::from(format!("data: {}\n\n", final_chunk)));
        };
        return sse_response(st);
    }

    let text = match generate(&prompt, model_id, think_mode, file_refs.as_deref(), extra.as_deref()).await {
        Ok(t) => t,
        Err(e) => return err_response(StatusCode::BAD_GATEWAY, &format!("upstream error: {e}")),
    };
    if text.is_empty() {
        log("Warning: empty response from Gemini");
    }

    let mut response_parts: Vec<Value> = Vec::new();
    if has_tools && !text.is_empty() {
        let (clean, fcs) = parse_google_function_calls(&text);
        if !fcs.is_empty() {
            if !clean.is_empty() {
                response_parts.push(json!({"text": clean}));
            }
            for fc in fcs {
                response_parts.push(json!({"functionCall": {"name": fc["name"], "args": fc["args"]}}));
            }
        } else {
            response_parts.push(json!({"text": text}));
        }
    } else {
        let t = if text.is_empty() {
            "I apologize, but I was unable to generate a response. Please try again.".to_string()
        } else {
            text.clone()
        };
        response_parts.push(json!({"text": t}));
    }

    let pt = tok(&prompt);
    let ct = tok(&text);
    let response_obj = json!({
        "candidates": [{"content": {"parts": response_parts, "role": "model"}, "finishReason": "STOP", "index": 0}],
        "usageMetadata": {"promptTokenCount": pt, "candidatesTokenCount": ct, "totalTokenCount": pt + ct},
        "modelVersion": model_name,
    });

    if stream {
        let st = async_stream::stream! {
            yield Ok::<_, std::io::Error>(Bytes::from(format!("data: {}\n\n", response_obj)));
        };
        return sse_response(st);
    }
    json_response(StatusCode::OK, response_obj)
}

// ─── Fallback router (handles dynamic /v1beta/models/...:method paths) ─────────

pub async fn fallback(method: Method, uri: Uri, _headers: HeaderMap, body: Bytes) -> Response {
    let path = uri.path().to_string();
    if method == Method::POST {
        if path.contains(":streamGenerateContent") {
            return handle_google_generate(&path, body, true).await;
        }
        if path.contains(":generateContent") {
            return handle_google_generate(&path, body, false).await;
        }
    }
    err_response(StatusCode::NOT_FOUND, "not found")
}
