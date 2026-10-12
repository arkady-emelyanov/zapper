//! The read-only web UI: one static page over the read-only API.

use axum::http::header;
use axum::response::IntoResponse;

pub async fn index() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], include_str!("ui.html"))
}
