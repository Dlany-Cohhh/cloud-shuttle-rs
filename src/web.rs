//! HTTP 层：静态前端、/api、/media/ 的 Range 流（在线播放）。
//! 只用 axum 的路由与 fallback，路径分发手写，避免不同 axum 版本通配符语法差异。

use crate::api;
use crate::app::AppState;
use crate::localfs;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn router(st: AppState) -> Router {
    Router::new()
        .route("/api", post(api_handler))
        .fallback(asset_handler)
        .with_state(st)
}

async fn api_handler(State(st): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    Json(api::dispatch(st, body).await)
}

fn mime_of(p: &Path) -> &'static str {
    match p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "eot" => "application/vnd.ms-fontobject",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        "txt" | "srt" | "vtt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h = (bytes[i + 1] as char).to_digit(16);
            let l = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (h, l) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn reply(status: StatusCode, mime: &str, body: Vec<u8>, cache: &str) -> Response {
    axum::http::Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, body.len())
        .header(header::CACHE_CONTROL, cache)
        .body(Body::from(body))
        .unwrap()
}

fn not_found(msg: &str) -> Response {
    reply(
        StatusCode::NOT_FOUND,
        "text/plain; charset=utf-8",
        msg.as_bytes().to_vec(),
        "no-store",
    )
}

async fn asset_handler(State(st): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let range = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());
    if path.starts_with("/media/") || path.starts_with("/media?") {
        return media(&st, &path, range.as_deref(), method == Method::HEAD).await;
    }
    if method != Method::GET && method != Method::HEAD {
        return reply(
            StatusCode::METHOD_NOT_ALLOWED,
            "text/plain",
            b"method not allowed".to_vec(),
            "no-store",
        );
    }
    let raw = percent_decode(path.trim_start_matches('/'));
    let rel = if raw.is_empty() {
        "index.html".to_string()
    } else {
        raw.clone()
    };
    // 兼容前端写 /static/xxx 或直接 /xxx 两种引用方式
    let rel = rel.strip_prefix("static/").unwrap_or(&rel).to_string();
    let root = match std::fs::canonicalize(&st.cfg.static_dir) {
        Ok(p) => p,
        Err(_) => {
            return reply(
                StatusCode::SERVICE_UNAVAILABLE,
                "text/html; charset=utf-8",
                format!("找不到前端静态资源目录：{}。请把 Python 版的 static/ 放到可执行文件同目录，或用 --static-dir 指定。", st.cfg.static_dir.display()).into_bytes(),
                "no-store",
            );
        }
    };
    let full = root.join(&rel);
    let canon = std::fs::canonicalize(&full).unwrap_or_else(|_| full.clone());
    if !canon.starts_with(&root) || !canon.is_file() {
        return not_found("404 Not Found");
    }
    let data = match tokio::fs::read(&canon).await {
        Ok(d) => d,
        Err(_) => return not_found("404 Not Found"),
    };
    let mime = mime_of(&canon);
    let cache = if rel == "index.html" || rel.ends_with(".html") {
        "no-cache"
    } else {
        "public, max-age=3600"
    };
    if method == Method::HEAD {
        return reply(StatusCode::OK, mime, Vec::new(), cache);
    }
    reply(StatusCode::OK, mime, data, cache)
}

fn parse_range(h: Option<&str>, len: u64) -> Option<(u64, u64)> {
    let h = h?;
    let spec = h.strip_prefix("bytes=")?;
    let mut it = spec.splitn(2, '-');
    let a = it.next()?.trim();
    let b = it.next().unwrap_or("").trim();
    if len == 0 {
        return Some((0, 0));
    }
    if a.is_empty() {
        let suffix: u64 = b.parse().ok()?;
        if suffix == 0 {
            return None;
        }
        let start = len.saturating_sub(suffix);
        Some((start, len - 1))
    } else {
        let start: u64 = a.parse().ok()?;
        if start >= len {
            return None;
        }
        let end = if b.is_empty() {
            len - 1
        } else {
            b.parse::<u64>().ok()?.min(len - 1)
        };
        if end < start {
            return None;
        }
        Some((start, end))
    }
}

async fn media(st: &AppState, path: &str, range: Option<&str>, head: bool) -> Response {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let rel = percent_decode(path.trim_start_matches("/media/"));
    let full: PathBuf = match localfs::safe_path(&st.cfg.download_root, &rel) {
        Ok(p) => p,
        Err(e) => return not_found(&e),
    };
    if !full.is_file() {
        return not_found("404 Not Found");
    }
    let mut f = match tokio::fs::File::open(&full).await {
        Ok(f) => f,
        Err(_) => return not_found("404 Not Found"),
    };
    let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
    let mime = mime_of(&full).to_string();
    let (status, start, end, body) = match parse_range(range, len) {
        Some((a, b)) if !head => {
            if f.seek(std::io::SeekFrom::Start(a)).await.is_err() {
                return not_found("seek failed");
            }
            let want = (b - a + 1).min(8 * 1024 * 1024) as usize;
            let mut buf = vec![0u8; want];
            let mut got = 0usize;
            while got < want {
                match f.read(&mut buf[got..]).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got += n,
                }
            }
            buf.truncate(got);
            (StatusCode::PARTIAL_CONTENT, a, a + got as u64 - 1, buf)
        }
        Some((a, b)) => (StatusCode::PARTIAL_CONTENT, a, b, Vec::new()),
        None if range.is_some() => (StatusCode::RANGE_NOT_SATISFIABLE, 0, 0, Vec::new()),
        None => (StatusCode::OK, 0, len.saturating_sub(1), Vec::new()),
    };
    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        return axum::http::Response::builder()
            .status(status)
            .header(header::CONTENT_RANGE, format!("bytes */{}", len))
            .body(Body::empty())
            .unwrap();
    }
    let partial = status == StatusCode::PARTIAL_CONTENT;
    let mut b = axum::http::Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_LENGTH, body.len());
    if partial {
        b = b.header(
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{}", start, end.max(start), len),
        );
    }
    b.body(Body::from(body)).unwrap()
}
