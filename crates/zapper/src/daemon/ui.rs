//! The web UI: one static page over the read routes and the approvals route.

use axum::http::header;
use axum::response::IntoResponse;

pub async fn index() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], include_str!("ui.html"))
}

/// xterm.js 6.0.0 (MIT), bundled; see `assets/README`.
/// A single terminal, read-only, for its own browser window.
pub async fn terminal() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], include_str!("terminal.html"))
}

pub async fn xterm_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("assets/xterm.js"))
}

pub async fn xterm_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], include_str!("assets/xterm.css"))
}
