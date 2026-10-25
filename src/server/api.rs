//! Settings page backend.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::error;
use crate::app::AppState;
use crate::autostart;
use crate::config::{AppSettings, LoggingSettings, ServerSettings, Settings};

pub async fn status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let registry = state.registry.read().unwrap().clone();
    let configured = state.settings.read().unwrap().providers.clone();
    let providers: Vec<_> = configured
        .iter()
        .map(|c| {
            let live = registry.provider(&c.id);
            let agents: Vec<String> = c.agents.iter().map(|a| format!("{}/{}", c.id, a.name)).collect();
            let discovered: Vec<String> = live
                .as_ref()
                .map(|p| {
                    p.discovered()
                        .into_iter()
                        .map(|m| format!("{}/{m}", c.id))
                        .filter(|m| !agents.contains(m))
                        .collect()
                })
                .unwrap_or_default();
            json!({ "id": c.id, "kind": c.kind, "enabled": c.enabled, "runtime": c.runtime, "command": c.command(),
                    "discover": c.discover_models, "max_prompt_chars": c.max_prompt_chars, "images": c.images,
                    "models": agents, "discovered": discovered })
        })
        .collect();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "base_url": state.base_url(),
        "config_file": state.paths.config_file,
        "logs_dir": state.paths.logs_dir,
        "autostart": autostart::is_enabled(),
        "providers": providers,
    }))
}

#[derive(Serialize, Deserialize)]
pub struct SettingsView {
    server: ServerSettings,
    app: AppSettings,
    logging: LoggingSettings,
    providers_toml: String,
}

pub async fn get_settings(State(state): State<Arc<AppState>>) -> Json<SettingsView> {
    let s = state.settings.read().unwrap().clone();
    Json(SettingsView {
        providers_toml: s.providers_toml(),
        server: s.server,
        app: s.app,
        logging: s.logging,
    })
}

pub async fn put_settings(
    State(state): State<Arc<AppState>>,
    Json(view): Json<SettingsView>,
) -> Response {
    let providers = match Settings::parse_providers_toml(&view.providers_toml) {
        Ok(p) => p,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("{e:#}"),
                "invalid_request_error",
                None,
            )
        }
    };
    let settings = Settings {
        server: view.server,
        app: view.app,
        logging: view.logging,
        providers,
    };
    match state.apply_settings(settings) {
        Ok(restart) => {
            Json(json!({ "restart": restart, "base_url": state.base_url() })).into_response()
        }
        Err(e) => error(
            StatusCode::BAD_REQUEST,
            format!("{e:#}"),
            "invalid_request_error",
            None,
        ),
    }
}

#[derive(Deserialize)]
pub struct LogsQuery {
    #[serde(default = "default_lines")]
    lines: usize,
}

fn default_lines() -> usize {
    200
}

pub async fn logs(State(state): State<Arc<AppState>>, Query(q): Query<LogsQuery>) -> Response {
    match state.log.tail(q.lines.clamp(1, 5000)) {
        Ok(text) => text.into_response(),
        Err(e) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            e.to_string(),
            "server_error",
            None,
        ),
    }
}

/// Re-runs model discovery of one provider and returns its full model list.
pub async fn discover(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let provider = state.registry.read().unwrap().provider(&id);
    let Some(provider) = provider else {
        return error(
            StatusCode::NOT_FOUND,
            format!("no enabled provider {id:?}"),
            "invalid_request_error",
            None,
        );
    };
    match provider.discover().await {
        Ok(_) => Json(json!({ "models": provider.model_ids() })).into_response(),
        Err(e) => error(
            StatusCode::BAD_GATEWAY,
            e.to_string(),
            "server_error",
            Some("provider_error"),
        ),
    }
}

pub async fn login(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let provider = state.registry.read().unwrap().deepseek(&id);
    let Some(provider) = provider else {
        return error(
            StatusCode::NOT_FOUND,
            format!("no deepseek-web provider {id:?}"),
            "invalid_request_error",
            None,
        );
    };
    match provider.login().await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => error(
            StatusCode::BAD_GATEWAY,
            e.to_string(),
            "server_error",
            Some("provider_error"),
        ),
    }
}
