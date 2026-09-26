use std::net::IpAddr;
use std::path::{Component, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, StatusCode, Uri, header, uri::Authority};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use tokio::sync::Mutex;

use super::catalog::Catalog;

#[derive(Clone)]
struct AppState {
    root: PathBuf,
    assets: PathBuf,
    catalog: Arc<Mutex<Option<CachedCatalog>>>,
}

struct CachedCatalog {
    value: Arc<Catalog>,
    loaded_at: Instant,
    refresh_error: Option<String>,
}

impl AppState {
    async fn snapshot(&self) -> Result<(Arc<Catalog>, Option<String>), ApiError> {
        let mut saved = self.catalog.lock().await;
        if let Some(cached) = saved.as_ref()
            && cached.loaded_at.elapsed() < Duration::from_secs(4)
        {
            return Ok((cached.value.clone(), cached.refresh_error.clone()));
        }
        let root = self.root.clone();
        let loaded = tokio::task::spawn_blocking(move || Catalog::load(&root))
            .await
            .map_err(ApiError::internal)
            .and_then(|result| result.map_err(ApiError::internal));
        let value = match loaded {
            Ok(catalog) => Arc::new(catalog),
            Err(error) => {
                if let Some(cached) = saved.as_mut() {
                    cached.loaded_at = Instant::now();
                    cached.refresh_error = Some(format!(
                        "Showing saved results; refresh failed: {}",
                        error.1
                    ));
                    return Ok((cached.value.clone(), cached.refresh_error.clone()));
                }
                return Err(error);
            }
        };
        *saved = Some(CachedCatalog {
            value: value.clone(),
            loaded_at: Instant::now(),
            refresh_error: None,
        });
        drop(saved);
        Ok((value, None))
    }
}

pub(super) fn router(root: PathBuf, assets: PathBuf) -> Router {
    let state = AppState {
        root,
        assets,
        catalog: Arc::new(Mutex::new(None)),
    };
    Router::new()
        .route("/api/catalog", get(catalog))
        .route("/api/attempts/{id}/logs/{name}", get(log))
        .fallback(get(asset))
        .layer(middleware::from_fn(local_request))
        .with_state(state)
}

async fn catalog(State(state): State<AppState>) -> Result<Response, ApiError> {
    let (snapshot, warning) = state.snapshot().await?;
    let mut value = serde_json::to_value(&*snapshot).map_err(ApiError::internal)?;
    if let Some(warning) = warning
        && let Some(warnings) = value
            .get_mut("warnings")
            .and_then(serde_json::Value::as_array_mut)
    {
        warnings.push(serde_json::Value::String(warning));
    }
    Ok(Json(value).into_response())
}

async fn log(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let (snapshot, _) = state.snapshot().await?;
    let content = tokio::task::spawn_blocking(move || snapshot.read_log(&id, &name))
        .await
        .map_err(ApiError::internal)?
        .map_err(|_error| ApiError(StatusCode::NOT_FOUND, String::from("Log is unavailable")))?;
    Ok(Json(content).into_response())
}

async fn asset(State(state): State<AppState>, uri: Uri) -> Result<Response, ApiError> {
    let relative = uri.path().strip_prefix('/').unwrap_or_default();
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    let path = PathBuf::from(relative);
    if !path
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(ApiError::not_found());
    }
    let path = tokio::fs::canonicalize(state.assets.join(path))
        .await
        .map_err(|_error| ApiError::not_found())?;
    if !path.starts_with(&state.assets) {
        return Err(ApiError::not_found());
    }
    let mime = match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        _ => return Err(ApiError::not_found()),
    };
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_error| ApiError::not_found())?;
    Ok(([(header::CONTENT_TYPE, mime)], Body::from(bytes)).into_response())
}

async fn local_request(request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(viewer_authority);
    let origin = request.headers().get(header::ORIGIN);
    if host.is_none()
        || origin.is_some_and(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.strip_prefix("http://"))
                .and_then(viewer_authority)
                != host
        })
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"));
    response
}

#[derive(Debug, PartialEq, Eq)]
enum ViewerHost {
    Name(String),
    Ip(IpAddr),
}

fn viewer_authority(value: &str) -> Option<(ViewerHost, u16)> {
    if value.contains('@') || value.ends_with(':') {
        return None;
    }
    let authority = value.parse::<Authority>().ok()?;
    let port = if authority.as_str() == authority.host() {
        80
    } else {
        authority.port_u16()?
    };
    let host = authority.host();
    let host = if let Some(host) = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        ViewerHost::Ip(IpAddr::V6(host.parse().ok()?))
    } else if let Ok(ip) = host.parse() {
        ViewerHost::Ip(IpAddr::V4(ip))
    } else if !host.is_empty()
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        ViewerHost::Name(host.to_ascii_lowercase())
    } else {
        return None;
    };
    (port != 0).then_some((host, port))
}

#[derive(Debug)]
struct ApiError(StatusCode, String);

impl ApiError {
    fn internal(error: impl std::fmt::Display) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }

    fn not_found() -> Self {
        Self(StatusCode::NOT_FOUND, String::from("Not found"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error": self.1}))).into_response()
    }
}

#[cfg(test)]
mod tests;
