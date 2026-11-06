//! NodeQuality 自建后端
//!
//! 只做三件事：收包、存包、发包。
//! 报告页的渲染全部在前端完成（xterm.js），后端保持极薄。

use std::{
    fs,
    io::{Read, Write},
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde::Serialize;
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

/// 上传体积上限：base64 会膨胀约 1/3，32 MB 对应约 24 MB 原始 zip。
const MAX_UPLOAD: usize = 32 * 1024 * 1024;

/// token 字符集，和上游保持一致的长度（32 位字母数字）。
const TOKEN_CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const TOKEN_LEN: usize = 32;

#[derive(Clone)]
struct App {
    data_dir: PathBuf,
    base_url: String,
}

#[derive(Serialize)]
struct ApiOk<T> {
    success: bool,
    data: T,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordData {
    token: String,
    asn: Option<String>,
    as_organization: Option<String>,
    location: Option<serde_json::Value>,
    /// base64 后的 zip，与上游 GET /api/v1/record/{token} 完全一致
    result: String,
    provider: Option<String>,
}

#[derive(Serialize, serde::Deserialize)]
struct Meta {
    created_at: u64,
    size: usize,
    provider: Option<String>,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// 用 /dev/urandom 生成 token，避开 rand crate 的版本差异。
fn gen_token() -> String {
    let mut buf = [0u8; TOKEN_LEN];
    let ok = fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok();
    if !ok {
        // 极端兜底：用纳秒时间，不可用于生产的多实例场景，但不会崩
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((nanos >> ((i % 16) * 8)) & 0xff) as u8;
        }
    }
    buf.iter()
        .map(|b| TOKEN_CHARSET[(*b as usize) % TOKEN_CHARSET.len()] as char)
        .collect()
}

fn is_zip(bytes: &[u8]) -> bool {
    bytes.len() > 4 && &bytes[..4] == b"PK\x03\x04"
}

fn token_of(p: &str) -> Result<&str, StatusCode> {
    if p.len() == TOKEN_LEN && p.bytes().all(|b| TOKEN_CHARSET.contains(&b)) {
        Ok(p)
    } else {
        Err(StatusCode::BAD_REQUEST)
    }
}

/// POST /api/v1/record
///
/// 请求体是 zip 的 base64（允许含换行），响应体是**纯文本链接**——
/// 客户端是 `base64 file | curl --data-binary @- URL`，直接把这个响应打到屏幕上。
async fn upload(State(app): State<Arc<App>>, body: Bytes) -> Response {
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty body").into_response();
    }

    let text = String::from_utf8_lossy(&body);
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let Ok(bytes) = B64.decode(cleaned.as_bytes()) else {
        return (StatusCode::BAD_REQUEST, "invalid base64").into_response();
    };
    if !is_zip(&bytes) {
        return (StatusCode::BAD_REQUEST, "payload is not a zip").into_response();
    }

    let token = gen_token();
    let dir = app.data_dir.join(&token);
    if let Err(e) = fs::create_dir_all(&dir) {
        tracing::error!(error = %e, "创建目录失败");
        return (StatusCode::INTERNAL_SERVER_ERROR, "storage error").into_response();
    }

    let meta = Meta {
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        size: bytes.len(),
        provider: None,
    };

    let meta_json = match serde_json::to_vec_pretty(&meta) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "序列化 meta 失败");
            return (StatusCode::INTERNAL_SERVER_ERROR, "storage error").into_response();
        }
    };

    if let Err(e) = fs::write(dir.join("report.zip"), &bytes)
        .and_then(|_| fs::write(dir.join("meta.json"), &meta_json))
    {
        tracing::error!(error = %e, "写盘失败");
        let _ = fs::remove_dir_all(&dir);
        return (StatusCode::INTERNAL_SERVER_ERROR, "storage error").into_response();
    }

    tracing::info!(token = %token, bytes = bytes.len(), "已保存报告");
    (StatusCode::OK, format!("{}/r/{}\n", app.base_url, token)).into_response()
}

/// GET /api/v1/record/{token} —— 与上游同构的读接口
async fn get_record(
    State(app): State<Arc<App>>,
    Path(token): Path<String>,
) -> Response {
    let Ok(token) = token_of(&token) else {
        return (StatusCode::BAD_REQUEST, "bad token").into_response();
    };
    let dir = app.data_dir.join(token);

    let Ok(bytes) = fs::read(dir.join("report.zip")) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    let meta: Option<Meta> = fs::read(dir.join("meta.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());

    let data = RecordData {
        token: token.to_string(),
        asn: None,
        as_organization: None,
        location: None,
        result: B64.encode(&bytes),
        provider: meta.and_then(|m| m.provider),
    };

    (
        StatusCode::OK,
        axum::Json(ApiOk {
            success: true,
            data,
        }),
    )
        .into_response()
}

async fn healthz() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .init();

    let addr: SocketAddr = env_or("NQ_ADDR", "0.0.0.0:8080").parse()?;
    let data_dir = PathBuf::from(env_or("NQ_DATA_DIR", "./data"));
    let web_dir = PathBuf::from(env_or("NQ_WEB_DIR", "./web"));
    let base_url = env_or("NQ_BASE_URL", &format!("http://{}", addr))
        .trim_end_matches('/')
        .to_string();

    fs::create_dir_all(&data_dir)?;

    let app_state = Arc::new(App {
        data_dir: data_dir.clone(),
        base_url: base_url.clone(),
    });

    // 前端构建产物：不存在时返回一个提示页，方便先跑后端
    let index = web_dir.join("index.html");
    let static_svc = ServeDir::new(&web_dir).fallback(ServeFile::new(index));

    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/api/v1/record", post(upload))
        .route("/api/v1/record/{token}", get(get_record))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .fallback_service(static_svc)
        .with_state(app_state);

    tracing::info!(%addr, data_dir = %data_dir.display(), web_dir = %web_dir.display(), base_url = %base_url, "服务启动");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;
    Ok(())
}

#[allow(dead_code)]
fn write_atomic(path: &FsPath, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
    }
    fs::rename(tmp, path)
}
