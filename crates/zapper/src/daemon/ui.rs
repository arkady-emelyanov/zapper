//! The web UI: the office page and the sessions page, over the read routes,
//! answering approvals, and the board's writes.

use axum::http::header;
use axum::response::IntoResponse;

/// The office: firms, mail, security, the shared drive, statistics.
pub async fn index() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], include_str!("office.html"))
}

/// Sessions, network, terminals and approvals.
pub async fn sessions() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], include_str!("ui.html"))
}

/// xterm.js 6.0.0 (MIT), bundled; see `assets/README`.
/// A single terminal, read-only, for its own browser window.
pub async fn terminal() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], include_str!("terminal.html"))
}

/// Claude's spark, in clay.
pub async fn favicon() -> impl IntoResponse {
    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><g stroke="#D97757" stroke-width="3.2" stroke-linecap="round"><path d="M16 3v26M3 16h26M6.8 6.8l18.4 18.4M25.2 6.8 6.8 25.2"/></g></svg>"##;
    ([(header::CONTENT_TYPE, "image/svg+xml")], SVG)
}

pub async fn xterm_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("assets/xterm.js"))
}

pub async fn xterm_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], include_str!("assets/xterm.css"))
}
