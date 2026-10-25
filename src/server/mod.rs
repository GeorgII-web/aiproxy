//! HTTP: `/v1/*` (OpenAI), `/api/*` and `/` (settings page).

mod api;
mod v1;

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tower_http::cors::CorsLayer;

use crate::app::AppState;
use crate::openai::{ApiError, ErrorBody};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(ui))
        .route("/health", get(|| async { "ok" }))
        .route("/api/status", get(api::status))
        .route("/api/settings", get(api::get_settings).put(api::put_settings))
        .route("/api/logs", get(api::logs))
        .route("/api/providers/{id}/login", post(api::login))
        .route("/v1/models", get(v1::models))
        .route("/v1/models/{*id}", get(v1::model))
        .route("/v1/chat/completions", post(v1::chat))
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .layer(middleware::from_fn(log_request))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn ui() -> Html<&'static str> {
    Html(include_str!("../../ui/index.html"))
}

pub fn error(status: StatusCode, message: impl Into<String>, kind: &'static str, code: Option<&'static str>) -> Response {
    let body = ErrorBody { error: ApiError { message: message.into(), kind, param: None, code } };
    (status, Json(body)).into_response()
}

/// With an api_key set, `/v1` and `/api` need `Authorization: Bearer <key>`.
async fn auth(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let key = state.settings.read().unwrap().server.api_key.clone();
    let path = req.uri().path();
    if key.is_empty() || !(path.starts_with("/v1") || path.starts_with("/api")) {
        return next.run(req).await;
    }
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|v| v.trim() == key);
    if ok {
        next.run(req).await
    } else {
        error(StatusCode::UNAUTHORIZED, "invalid api key", "invalid_request_error", Some("invalid_api_key"))
    }
}

async fn log_request(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let start = Instant::now();
    let res = next.run(req).await;
    tracing::info!(%method, %path, status = res.status().as_u16(), ms = start.elapsed().as_millis() as u64, "request");
    res
}
