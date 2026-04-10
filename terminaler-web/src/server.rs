use crate::auth;
use crate::bridge::MuxBridge;
use crate::ws_session;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
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
const TERMINAL_HTML: &str = include_str!("../static/terminal.html");

/// Build the axum router.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/terminal", get(terminal_handler))
        .route("/xterm.min.js", get(xterm_js_handler))
        .route("/xterm.css", get(xterm_css_handler))
        .route("/ws", get(ws_handler))
        .with_state(state)
}

/// Serve the main HTML page (requires valid token).
async fn index_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Query<HashMap<String, String>>,
) -> Response {
    match auth::authorize_page_request(&headers, &query, &state.token, "/") {
        Ok(auth::PageAuthResult::Authorized) => Html(INDEX_HTML).into_response(),
        Ok(auth::PageAuthResult::Redirect(resp)) => resp,
        Err(resp) => resp,
    }
}

/// Serve the custom terminal HTML page (requires valid token).
async fn terminal_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Query<HashMap<String, String>>,
) -> Response {
    match auth::authorize_page_request(&headers, &query, &state.token, "/terminal") {
        Ok(auth::PageAuthResult::Authorized) => Html(TERMINAL_HTML).into_response(),
        Ok(auth::PageAuthResult::Redirect(resp)) => resp,
        Err(resp) => resp,
    }
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

/// WebSocket upgrade handler (requires valid token).
async fn ws_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if let Err(resp) = auth::authorize_ws_request(&headers, &state.token) {
        return resp;
    }
    let bridge = state.bridge.clone();
    ws.on_upgrade(move |socket| ws_session::handle_ws(socket, bridge))
}
