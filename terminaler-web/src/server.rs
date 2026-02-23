use crate::auth;
use crate::bridge::MuxBridge;
use crate::ws_session;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::collections::HashMap;
use std::sync::Arc;

/// Shared state for the axum server.
#[derive(Clone)]
pub struct AppState {
    pub token: Arc<String>,
    pub bridge: Arc<MuxBridge>,
}

// Embedded static files
const INDEX_HTML: &str = include_str!("../static/index.html");
const XTERM_JS: &str = include_str!("../static/xterm.min.js");
const XTERM_CSS: &str = include_str!("../static/xterm.css");
const XTERM_FIT_JS: &str = include_str!("../static/xterm-addon-fit.min.js");

/// Build the axum router.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/xterm.min.js", get(xterm_js_handler))
        .route("/xterm.css", get(xterm_css_handler))
        .route("/xterm-addon-fit.min.js", get(xterm_fit_handler))
        .route("/ws", get(ws_handler))
        .with_state(state)
}

/// Serve the main HTML page (requires valid token).
async fn index_handler(
    State(state): State<AppState>,
    query: Query<HashMap<String, String>>,
) -> Response {
    if let Err(resp) = auth::check_token(&query, &state.token) {
        return resp;
    }
    Html(INDEX_HTML).into_response()
}

/// Serve xterm.js (no auth needed since it's a static asset and HTML already requires auth).
async fn xterm_js_handler() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        XTERM_JS,
    )
}

/// Serve xterm.css.
async fn xterm_css_handler() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/css")],
        XTERM_CSS,
    )
}

/// Serve xterm fit addon.
async fn xterm_fit_handler() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        XTERM_FIT_JS,
    )
}

/// WebSocket upgrade handler (requires valid token).
async fn ws_handler(
    State(state): State<AppState>,
    query: Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    if let Err(resp) = auth::check_token(&query, &state.token) {
        return resp;
    }
    let bridge = state.bridge.clone();
    ws.on_upgrade(move |socket| ws_session::handle_ws(socket, bridge))
}
