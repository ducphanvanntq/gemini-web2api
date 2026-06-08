//! Tool calling and multimodal message parsing. Mirrors gemini_web2api/tools.py.
use self::base64_decode as b64;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};

/// A decoded image: (raw bytes, mime type).
pub type ImageItem = (Vec<u8>, String);

// ─── tool_choice helpers ─────────────────────────────────────────────────────

/// Build a tool_choice constraint instruction. `tool_choice` is the raw JSON value.
fn build_tool_choice_instruction(tool_choice: &Value) -> String {
    if tool_choice == "none" {
        return "\n\nIMPORTANT: Do NOT call any tools. Respond with text only.".to_string();
    }
    if tool_choice == "required" {
        return "\n\nIMPORTANT: You MUST call at least one tool. Do not respond with text only."
            .to_string();
    }
    if let Some(obj) = tool_choice.as_object() {
        if let Some(name) = obj.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()) {
            if !name.is_empty() {
                return format!(
                    "\n\nIMPORTANT: You MUST call the tool \"{name}\". Do not call other tools."
                );
            }
        }
    }
    String::new()
}

/// Extract a tool definition {name, description, parameters} from an OpenAI tool entry.
fn extract_tool_def(tool: &Value) -> Value {
    let is_function = tool.get("type").and_then(|t| t.as_str()) == Some("function");
    let fn_obj = if is_function {
        tool.get("function").unwrap_or(tool)
    } else {
        tool
    };
    let pick = |key: &str| -> Value {
        fn_obj
            .get(key)
            .cloned()
            .or_else(|| tool.get(key).cloned())
            .unwrap_or(Value::Null)
    };
    json!({
        "name": match pick("name") { Value::Null => json!(""), v => v },
        "description": match pick("description") { Value::Null => json!(""), v => v },
        "parameters": match pick("parameters") { Value::Null => json!({}), v => v },
    })
}

/// Convert OpenAI messages to (prompt_str, images). Images are decoded (bytes, mime).
///
/// Note: the OpenAI path does not support image input (mirrors Python — image_url
/// parts become a note and are not uploaded), so `images` is always empty here.
pub fn messages_to_prompt(
    messages: &[Value],
    tools: Option<&Vec<Value>>,
    tool_choice: &Value,
) -> (String, Vec<ImageItem>) {
    let mut parts: Vec<String> = Vec::new();
    let images: Vec<ImageItem> = Vec::new();

    if let Some(tools) = tools {
        if tool_choice != "none" && !tools.is_empty() {
            let tool_defs: Vec<Value> = tools.iter().map(extract_tool_def).collect();
            let constraint = build_tool_choice_instruction(tool_choice);
            let defs_str = serde_json::to_string_pretty(&Value::Array(tool_defs)).unwrap();
            parts.push(format!(
                "# Tool Use\n\n\
You can call the following tools. Call format:\n\
```tool_call\n{{\"name\": \"func_name\", \"arguments\": {{...}}}}\n```\n\
When calling tools, output ONLY the tool_call block(s).\n\n\
Available tools:\n{defs_str}{constraint}"
            ));
        }
    }

    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let content = msg.get("content").cloned().unwrap_or(Value::Null);

        let content_str = flatten_content(&content);

        match role {
            "system" => parts.push(format!("[System instruction]: {content_str}")),
            "assistant" => {
                if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    let mut tc_strs = Vec::new();
                    for tc in tcs {
                        let fnobj = tc.get("function").cloned().unwrap_or(Value::Null);
                        let name = fnobj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let args = fnobj
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}");
                        tc_strs.push(format!(
                            "```tool_call\n{{\"name\": \"{name}\", \"arguments\": {args}}}\n```"
                        ));
                    }
                    parts.push(format!("[Assistant]: {content_str}\n{}", tc_strs.join("\n")));
                } else {
                    parts.push(format!("[Assistant]: {content_str}"));
                }
            }
            "tool" => {
                let name = msg.get("name").and_then(|v| v.as_str()).unwrap_or("");
                parts.push(format!("[Tool result for {name}]: {content_str}"));
            }
            _ => {
                if !content_str.is_empty() {
                    parts.push(content_str);
                }
            }
        }
    }

    let prompt = parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n\n");
    (prompt, images)
}

/// Flatten OpenAI content which may be a string or a list of parts.
fn flatten_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => {
            let mut text_parts = Vec::new();
            for c in arr {
                let ctype = c.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match ctype {
                    "text" | "input_text" => {
                        text_parts.push(c.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string());
                    }
                    "image_url" | "image" => {
                        text_parts.push("[Note: Image input not supported in this API. Please describe the image in text.]".to_string());
                    }
                    _ => {}
                }
            }
            text_parts.join(" ")
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Extract tool_call blocks. Returns (clean_text, tool_calls as OpenAI JSON).
static RE_TOOL_CALL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)```tool_call\s*\n(.*?)\n```").unwrap());

pub fn parse_tool_calls(text: &str) -> (String, Vec<Value>) {
    let mut tool_calls = Vec::new();
    let mut clean_parts = String::new();
    let mut last_end = 0usize;

    for cap in RE_TOOL_CALL.captures_iter(text) {
        let whole = cap.get(0).unwrap();
        clean_parts.push_str(&text[last_end..whole.start()]);
        last_end = whole.end();
        let inner = cap.get(1).unwrap().as_str().trim();
        if let Ok(data) = serde_json::from_str::<Value>(inner) {
            if let Some(name) = data.get("name").and_then(|v| v.as_str()) {
                let args = data.get("arguments").cloned().unwrap_or(json!({}));
                tool_calls.push(json!({
                    "id": format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(&args).unwrap(),
                    }
                }));
            }
        }
    }
    clean_parts.push_str(&text[last_end..]);
    (clean_parts.trim().to_string(), tool_calls)
}

// ─── Google native API helpers ───────────────────────────────────────────────

pub fn build_tool_prompt(tool_defs: &[Value]) -> String {
    let tool_spec = serde_json::to_string_pretty(&Value::Array(tool_defs.to_vec())).unwrap();
    format!(
        "# Tool Use\n\n\
You can call the following tools to help accomplish tasks. \
These tools connect to the user's local environment and will execute when called.\n\n\
Call format (use this exact format):\n\
```function_call\n{{\"name\": \"<tool_name>\", \"args\": {{<arguments>}}}}\n```\n\n\
When calling tools:\n\
- Output ONLY the function_call block(s), nothing else\n\
- You may call multiple tools with multiple blocks\n\
- After receiving a [Tool result for ...], use that data to answer the user\n\n\
Available tools:\n{tool_spec}"
    )
}

fn google_tool_choice_instruction(req: &Value) -> String {
    let fc = req.get("toolConfig").and_then(|v| v.get("functionCallingConfig"));
    let mode = fc.and_then(|v| v.get("mode")).and_then(|v| v.as_str()).unwrap_or("AUTO");
    let allowed: Vec<String> = fc
        .and_then(|v| v.get("allowedFunctionNames"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    match mode {
        "NONE" => "\n\nIMPORTANT: Do NOT call any tools. Respond with text only.".to_string(),
        "ANY" => {
            if !allowed.is_empty() {
                let names = allowed.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ");
                format!("\n\nIMPORTANT: You MUST call one of these tools: {names}. Do not respond with text only.")
            } else {
                "\n\nIMPORTANT: You MUST call at least one tool. Do not respond with text only.".to_string()
            }
        }
        _ => String::new(),
    }
}

/// Convert Google API request (contents/tools/systemInstruction) to (prompt, images).
pub fn google_contents_to_prompt(req: &Value) -> (String, Vec<ImageItem>) {
    let mut parts: Vec<String> = Vec::new();
    let mut images: Vec<ImageItem> = Vec::new();

    let fc_mode = req
        .get("toolConfig")
        .and_then(|v| v.get("functionCallingConfig"))
        .and_then(|v| v.get("mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("AUTO");

    let mut tool_defs: Vec<Value> = Vec::new();
    if fc_mode != "NONE" {
        if let Some(tools) = req.get("tools").and_then(|v| v.as_array()) {
            for tool_group in tools {
                if let Some(decls) = tool_group.get("functionDeclarations").and_then(|v| v.as_array()) {
                    for f in decls {
                        let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let desc = f.get("description").and_then(|v| v.as_str()).unwrap_or("");
                        let mut td = json!({"name": name, "description": desc});
                        if let Some(params) = f.get("parameters").or_else(|| f.get("parametersJsonSchema")) {
                            if !params.is_null() {
                                td["parameters"] = params.clone();
                            }
                        }
                        tool_defs.push(td);
                    }
                }
            }
        }
    }

    let sys_inst = req.get("systemInstruction");
    let sys_text = sys_inst
        .and_then(|s| s.get("parts"))
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();

    if !sys_text.is_empty() {
        if !tool_defs.is_empty() {
            let constraint = google_tool_choice_instruction(req);
            parts.push(format!("{sys_text}\n\n{}{constraint}", build_tool_prompt(&tool_defs)));
        } else {
            parts.push(sys_text);
        }
    } else if !tool_defs.is_empty() {
        let constraint = google_tool_choice_instruction(req);
        parts.push(format!("{}{constraint}", build_tool_prompt(&tool_defs)));
    }

    if let Some(contents) = req.get("contents").and_then(|v| v.as_array()) {
        for content in contents {
            let role = content.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let mut msg_parts: Vec<String> = Vec::new();
            if let Some(cparts) = content.get("parts").and_then(|v| v.as_array()) {
                for p in cparts {
                    if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                        if !t.is_empty() {
                            msg_parts.push(t.to_string());
                            continue;
                        }
                    }
                    if let Some(idata) = p.get("inlineData") {
                        let mime = idata.get("mimeType").and_then(|v| v.as_str()).unwrap_or("image/png");
                        if let Some(data) = idata.get("data").and_then(|v| v.as_str()) {
                            if let Some(bytes) = b64::decode(data) {
                                images.push((bytes, mime.to_string()));
                            }
                        }
                        continue;
                    }
                    if let Some(fc) = p.get("functionCall") {
                        let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let args = fc.get("args").cloned().unwrap_or(json!({}));
                        msg_parts.push(format!(
                            "```function_call\n{}\n```",
                            serde_json::to_string(&json!({"name": name, "args": args})).unwrap()
                        ));
                        continue;
                    }
                    if let Some(fr) = p.get("functionResponse") {
                        let name = fr.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let response = fr.get("response").cloned().unwrap_or(json!({}));
                        msg_parts.push(format!(
                            "[Tool result for {name}]: {}",
                            serde_json::to_string(&response).unwrap()
                        ));
                    }
                }
            }
            let text = msg_parts.join("\n");
            if role == "model" {
                parts.push(format!("[Assistant]: {text}"));
            } else {
                parts.push(text);
            }
        }
    }

    let prompt = parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n\n");
    (prompt, images)
}

/// Extract function_call blocks from model output. Returns (clean_text, calls).
static RE_FC1: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)```function_call\s*\n(.*?)\n```").unwrap());
static RE_FC2: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)(?:^|\n)function_call\s*\n(\{[^`]*?\})").unwrap());

pub fn parse_google_function_calls(text: &str) -> (String, Vec<Value>) {
    let mut function_calls: Vec<Value> = Vec::new();
    let mut clean = text.to_string();

    for re in [&*RE_FC1, &*RE_FC2] {
        for cap in re.captures_iter(&clean.clone()) {
            let inner = cap.get(1).unwrap().as_str().trim();
            if let Ok(data) = serde_json::from_str::<Value>(inner) {
                if let Some(name) = data.get("name").and_then(|v| v.as_str()) {
                    let args = data
                        .get("args")
                        .or_else(|| data.get("arguments"))
                        .cloned()
                        .unwrap_or(json!({}));
                    function_calls.push(json!({"name": name, "args": args}));
                }
            }
        }
        clean = re.replace_all(&clean, "").trim().to_string();
    }

    if function_calls.is_empty() && clean.trim_start().starts_with('{') {
        if let Ok(data) = serde_json::from_str::<Value>(clean.trim()) {
            let has_args = data.get("args").is_some() || data.get("arguments").is_some();
            if let Some(name) = data.get("name").and_then(|v| v.as_str()) {
                if has_args {
                    let args = data
                        .get("args")
                        .or_else(|| data.get("arguments"))
                        .cloned()
                        .unwrap_or(json!({}));
                    function_calls.push(json!({"name": name, "args": args}));
                    clean = String::new();
                }
            }
        }
    }

    (clean, function_calls)
}

/// Minimal base64 decoder (standard alphabet, ignores whitespace and padding).
mod base64_decode {
    pub fn decode(input: &str) -> Option<Vec<u8>> {
        fn val(c: u8) -> Option<u8> {
            match c {
                b'A'..=b'Z' => Some(c - b'A'),
                b'a'..=b'z' => Some(c - b'a' + 26),
                b'0'..=b'9' => Some(c - b'0' + 52),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            }
        }
        let mut out = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0u32;
        for &c in input.as_bytes() {
            if c == b'=' || c.is_ascii_whitespace() {
                continue;
            }
            let v = val(c)? as u32;
            buf = (buf << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        Some(out)
    }
}
