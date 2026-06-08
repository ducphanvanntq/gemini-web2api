//! Configuration management. Mirrors gemini_web2api/config.py.
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub port: u16,
    pub host: String,
    pub retry_attempts: u32,
    pub retry_delay_sec: u64,
    pub request_timeout_sec: u64,
    pub gemini_bl: String,
    /// account index; may be string or number in JSON, or null
    pub auth_user: Option<serde_json::Value>,
    pub xsrf_token: Option<String>,
    pub default_model: String,
    pub log_requests: bool,
    pub cookie_file: Option<String>,
    pub proxy: Option<String>,
    pub api_keys: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: 8081,
            host: "0.0.0.0".to_string(),
            retry_attempts: 3,
            retry_delay_sec: 2,
            request_timeout_sec: 180,
            gemini_bl: "boq_assistant-bard-web-server_20260525.09_p0".to_string(),
            auth_user: None,
            xsrf_token: None,
            default_model: "gemini-3.5-flash".to_string(),
            log_requests: true,
            cookie_file: None,
            proxy: None,
            api_keys: Vec::new(),
        }
    }
}

impl Config {
    /// Return the auth_user as a string, treating null / empty as None.
    pub fn auth_user_str(&self) -> Option<String> {
        match &self.auth_user {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(s)) if s.is_empty() => None,
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(v) => Some(v.to_string()),
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

pub fn config() -> &'static Config {
    CONFIG.get().expect("config not initialized")
}

pub fn init_config(c: Config) {
    let _ = CONFIG.set(c);
}

/// Load config from a JSON file, falling back to defaults for missing fields.
pub fn load_config(path: &str) -> Config {
    match std::fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str::<Config>(&content) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Failed to parse config {path}: {e}; using defaults");
                Config::default()
            }
        },
        Err(_) => Config::default(),
    }
}

/// Search for a config file in standard locations.
pub fn find_config() -> Option<String> {
    let mut candidates = vec!["./config.json".to_string()];
    if let Some(home) = dirs_home() {
        candidates.push(format!("{home}/.config/gemini-web2api/config.json"));
    }
    candidates.into_iter().find(|p| std::path::Path::new(p).exists())
}

fn dirs_home() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
}
