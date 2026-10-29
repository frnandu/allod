use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

#[cfg(feature = "embed-ui")]
#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/dist"]
struct Assets;

/// Serves the single-page app: known files as-is, every other non-API path as
/// `index.html` so client-side routes work on reload.
pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path == "api" || path.starts_with("api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve_asset(path)
}

#[cfg(feature = "embed-ui")]
fn serve_asset(path: &str) -> Response {
    let (file, name) = match Assets::get(path) {
        Some(file) if !path.is_empty() => (file, path),
        _ => match Assets::get("index.html") {
            Some(file) => (file, "index.html"),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
    };
    let mime = mime_guess::from_path(name).first_or_octet_stream();
    // Hashed assets are immutable; index.html must always be revalidated.
    let cache = if name.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [(header::CONTENT_TYPE, mime.as_ref().to_string()), (header::CACHE_CONTROL, cache.into())],
        file.data.into_owned(),
    )
        .into_response()
}

#[cfg(not(feature = "embed-ui"))]
fn serve_asset(_path: &str) -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        "<!doctype html><title>Allod</title><p>Built without the web UI (feature embed-ui).</p>",
    )
        .into_response()
}
