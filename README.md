# gemini-web2api (Rust port)

A 1-to-1 Rust port of the Python `gemini-web2api`. Converts the Google Gemini web
interface into an OpenAI-compatible API. Compiles to a single static binary — no
Python or runtime dependencies needed.

## Build

```bash
cargo build --release
```

The binary lands at `target/release/gemini-web2api`.

## Run

```bash
cp ../config.example.json config.json   # or write your own
./target/release/gemini-web2api --config config.json
```

Server starts at `http://localhost:8081/v1` (or `--port`).

### CLI flags

| Flag | Description |
|------|-------------|
| `--config <path>` | Path to `config.json` (else `$GEMINI_WEB2API_CONFIG`, then `./config.json`, then `~/.config/gemini-web2api/config.json`) |
| `--port <n>` | Override listen port |
| `--cookie-file <path>` | Override cookie file |
| `--proxy <url>` | HTTP proxy, e.g. `http://127.0.0.1:7890` |

## Endpoints (same as the Python version)

- `GET  /` — status
- `GET  /v1/models` — OpenAI model list
- `POST /v1/chat/completions` — OpenAI chat (streaming + tool calling)
- `POST /v1/responses` — OpenAI Responses API (Codex CLI)
- `GET  /v1beta/models` — Google model list
- `POST /v1beta/models/{model}:generateContent` — Google native (non-stream)
- `POST /v1beta/models/{model}:streamGenerateContent` — Google native (stream)

## Config

Identical schema to the Python project's `config.json`. See the parent
[../README.md](../README.md) for cookies, proxy, `auth_user`/`xsrf_token`, and model details.

## Module map (mirrors the Python package)

| Rust | Python | Role |
|------|--------|------|
| `src/config.rs` | `config.py` | Config struct + load/find |
| `src/models.rs` | `models.py` | Model table + `resolve_model` |
| `src/gemini.rs` | `gemini.py` | StreamGenerate protocol, payload, parsing, streaming, SAPISIDHASH |
| `src/tools.rs` | `tools.py` | Message→prompt, tool calling, Google helpers |
| `src/multimodal.rs` | `multimodal.py` | Scotty resumable image upload |
| `src/server.rs` | `server.py` | axum HTTP handlers |
| `src/main.rs` | `__main__.py` | CLI + startup |

## Stack

`tokio` + `axum` (HTTP server/SSE) · `reqwest` (upstream client) ·
`serde_json` (protocol JSON) · `regex` · `sha1` · `clap`.

## Releases & Docker image (CI/CD)

A manual GitHub Actions workflow ([.github/workflows/release.yml](.github/workflows/release.yml))
builds everything on demand:

1. Go to the repo **Actions** tab → **release** → **Run workflow**.
2. Enter a `tag` (e.g. `v1.1.0`) and choose whether to push the Docker image.

It produces:

- **Downloadable binaries** attached to a GitHub Release — Linux (x86_64),
  Windows (x86_64), macOS (arm64 + x86_64), each packaged with `config.example.json`.
- **A Docker image** pushed to GHCR: `ghcr.io/<owner>/gemini-web2api:<tag>` and `:latest`.

### Use a released binary

Download the archive for your OS from the Releases page, extract, then:

```bash
cp config.example.json config.json
./gemini-web2api --config config.json
```

> Linux binaries are built with OpenSSL (native-tls); a recent `libssl` must be
> present (it is on most distros). The Docker image ships it already.

### Pull the Docker image

```bash
docker run -d -p 8081:8081 \
  -v ./config.json:/app/config.json \
  ghcr.io/<owner>/gemini-web2api:latest
```

(Make the GHCR package public in the repo's Packages settings if you want anonymous pulls.)

## Differences from the Python version

- Token counts use Unicode scalar count / 4 (Python uses `len()`); negligible.
- The OpenAI image-input path is ignored exactly as in Python; image upload is
  reachable only via the Google native `inlineData` path.
- CORS is handled by `tower-http`'s permissive layer.
