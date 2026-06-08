//! Model definitions and mapping. Mirrors gemini_web2api/models.py.
//!
//! MODE_CATEGORY enum from Gemini frontend JS:
//!   1=FAST, 2=THINKING, 3=PRO, 4=AUTO, 5=FAST_DYNAMIC_THINKING, 6=FLASH_LITE
use once_cell::sync::Lazy;

#[derive(Clone)]
pub struct ModelCfg {
    pub mode: i64,
    pub think: i64,
    /// extra inner-payload fields as (index, value)
    pub extra: Option<Vec<(usize, i64)>>,
    pub desc: &'static str,
}

/// Ordered list of (name, cfg) preserving the Python dict insertion order.
pub static MODELS: Lazy<Vec<(&'static str, ModelCfg)>> = Lazy::new(|| {
    vec![
        ("gemini-3.5-flash", ModelCfg { mode: 1, think: 4, extra: None, desc: "Fast general-purpose model" }),
        ("gemini-3.5-flash-thinking", ModelCfg { mode: 2, think: 0, extra: None, desc: "Deep thinking mode, longest output (~20k chars)" }),
        ("gemini-3.1-pro", ModelCfg { mode: 3, think: 4, extra: None, desc: "Pro model (requires cookie for real routing)" }),
        ("gemini-3.1-pro-enhanced", ModelCfg { mode: 3, think: 4, extra: Some(vec![(31, 2), (80, 3)]), desc: "Pro with enhanced output (experimental)" }),
        ("gemini-auto", ModelCfg { mode: 4, think: 4, extra: None, desc: "Auto model selection" }),
        ("gemini-3.5-flash-thinking-lite", ModelCfg { mode: 5, think: 0, extra: None, desc: "Dynamic thinking with adaptive depth" }),
        ("gemini-flash-lite", ModelCfg { mode: 6, think: 4, extra: None, desc: "Lightweight fast model" }),
    ]
});

pub fn get_model(name: &str) -> Option<&'static ModelCfg> {
    MODELS.iter().find(|(n, _)| *n == name).map(|(_, c)| c)
}

pub struct Resolved {
    pub name: String,
    pub mode: i64,
    pub think: i64,
    pub extra: Option<Vec<(usize, i64)>>,
}

/// Resolve a model name to its configuration.
///
/// Returns Ok(Resolved) or Err(message). Unknown names fall back to `default`
/// rather than erroring, matching the Python behaviour.
pub fn resolve_model(model_name: &str, default: &str) -> Result<Resolved, String> {
    let mut name = model_name.to_string();
    let mut think_override: Option<i64> = None;

    if let Some(idx) = name.rfind("@think=") {
        let think_str = name[idx + "@think=".len()..].to_string();
        let base = name[..idx].to_string();
        match think_str.parse::<i64>() {
            Ok(v) => {
                think_override = Some(v);
                name = base;
            }
            Err(_) => return Err(format!("Invalid think level: {think_str}")),
        }
    }

    let cfg = match get_model(&name) {
        Some(c) => c,
        None => {
            crate::gemini::log(&format!("Unknown model '{name}', falling back to '{default}'"));
            name = default.to_string();
            get_model(default).expect("default model must exist")
        }
    };

    Ok(Resolved {
        name,
        mode: cfg.mode,
        think: think_override.unwrap_or(cfg.think),
        extra: cfg.extra.clone(),
    })
}
