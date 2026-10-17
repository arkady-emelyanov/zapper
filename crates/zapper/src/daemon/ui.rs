//! The web UI: the office page and the sessions page, over the read routes,
//! answering approvals, and the board's writes.

use axum::http::header;
use axum::response::IntoResponse;

/// The office: firms, mail, security, the shared drive, statistics.
pub async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("office.html"),
    )
}

/// Sessions, network, terminals and approvals.
pub async fn sessions() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("ui.html"),
    )
}

/// xterm.js 6.0.0 (MIT), bundled; see `assets/README`.
/// A single terminal, read-only, for its own browser window.
pub async fn terminal() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("terminal.html"),
    )
}

/// The firm's logo, as on the product's landing page and in the UI's header.
pub async fn favicon() -> impl IntoResponse {
    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><g fill="none" stroke="#D97757" stroke-width="5" stroke-linejoin="round" stroke-linecap="round"><circle cx="32" cy="32" r="28.5"/><path d="M42.16 36.21L36.21 42.16L27.79 42.16L21.84 36.21L21.84 27.79L27.79 21.84L36.21 21.84L42.16 27.79Z"/><path d="M42.16 36.21L42.16 60.23M36.21 42.16L19.23 59.15M27.79 42.16L3.77 42.16M21.84 36.21L4.85 19.23M21.84 27.79L21.84 3.77M27.79 21.84L44.77 4.85M36.21 21.84L60.23 21.84M42.16 27.79L59.15 44.77"/></g></svg>"##;
    ([(header::CONTENT_TYPE, "image/svg+xml")], SVG)
}

pub async fn xterm_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("assets/xterm.js"),
    )
}

pub async fn xterm_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("assets/xterm.css"),
    )
}
