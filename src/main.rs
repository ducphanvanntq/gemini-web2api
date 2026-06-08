//! Entry point. Mirrors gemini_web2api/__main__.py.
mod config;
mod gemini;
mod models;
mod multimodal;
mod server;
mod tools;

use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use config::{config, find_config, init_config, load_config, Config};
use models::MODELS;
use tower_http::cors::CorsLayer;

#[derive(Parser)]
#[command(name = "gemini-web2api", version = server::VERSION, about = "Gemini Web to OpenAI API")]
struct Args {
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    config: Option<String>,
    #[arg(long = "cookie-file")]
    cookie_file: Option<String>,
    #[arg(long, help = "HTTP proxy, e.g. http://127.0.0.1:7890")]
    proxy: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let config_path = args
        .config
        .clone()
        .or_else(|| std::env::var("GEMINI_WEB2API_CONFIG").ok())
        .or_else(find_config);

    let mut cfg: Config = match config_path {
        Some(p) => load_config(&p),
        None => Config::default(),
    };

    if let Some(port) = args.port {
        cfg.port = port;
    }
    if let Some(cf) = args.cookie_file {
        cfg.cookie_file = Some(cf);
    }
    if let Some(px) = args.proxy {
        cfg.proxy = Some(px);
    }

    let port = cfg.port;
    let host = cfg.host.clone();
    let has_cookie = cfg.cookie_file.is_some();
    let proxy_label = cfg.proxy.clone().unwrap_or_else(|| "system env".to_string());

    init_config(cfg);
    // Force the lazy HTTP client to build now (uses proxy from config).
    let _ = &*gemini::HTTP;

    let app = Router::new()
        .route("/", get(server::get_root))
        .route("/v1/models", get(server::get_models))
        .route("/v1beta/models", get(server::get_v1beta_models))
        .route("/v1/chat/completions", post(server::post_chat))
        .route("/v1/responses", post(server::post_responses))
        .fallback(server::fallback)
        .layer(CorsLayer::permissive());

    let model_names: Vec<&str> = MODELS.iter().map(|(n, _)| *n).collect();
    println!("gemini-web2api v{} (rust)", server::VERSION);
    println!("  Listening: http://{host}:{port}");
    println!("  Base URL:  http://localhost:{port}/v1");
    println!("  Models:    {}", model_names.join(", "));
    println!("  Cookie:    {}", if has_cookie { "yes" } else { "none (anonymous)" });
    println!("  Proxy:     {proxy_label}");
    println!();
    let _ = config(); // ensure initialized

    let listener = tokio::net::TcpListener::bind((host.as_str(), port))
        .await
        .unwrap_or_else(|e| panic!("failed to bind {host}:{port}: {e}"));
    axum::serve(listener, app).await.expect("server error");
}
