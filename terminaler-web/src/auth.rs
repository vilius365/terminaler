use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Response;
use std::collections::HashMap;
use std::sync::Arc;

/// Generate a random 32-byte hex token for authentication.
pub fn generate_token() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: [u8; 32] = rng.random();
    hex::encode(bytes)
}

/// Load or create a persistent token file.
/// Returns the token string.
pub fn load_or_create_token(configured_token: Option<&str>) -> anyhow::Result<String> {
    if let Some(token) = configured_token {
        return Ok(token.to_string());
    }

    let token_path = token_file_path()?;
    if token_path.exists() {
        let token = std::fs::read_to_string(&token_path)?.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    let token = generate_token();
    if let Some(parent) = token_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&token_path, &token)?;
    Ok(token)
}

fn token_file_path() -> anyhow::Result<std::path::PathBuf> {
    if let Some(ref dir) = *config::PORTABLE_DIR {
        return Ok(dir.join("web-token"));
    }
    if cfg!(windows) {
        let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        Ok(std::path::PathBuf::from(appdata)
            .join("Terminaler")
            .join("web-token"))
    } else {
        Ok(dirs_next::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".config"))
            .join("terminaler")
            .join("web-token"))
    }
}

/// Validate the token from a query parameter against the expected token.
pub fn validate_token(
    query: &Query<HashMap<String, String>>,
    expected: &str,
) -> Result<(), StatusCode> {
    match query.get("token") {
        Some(token) if token == expected => Ok(()),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Axum middleware-style token validation for use in handlers.
pub fn check_token(
    query: &Query<HashMap<String, String>>,
    token: &Arc<String>,
) -> Result<(), Response> {
    validate_token(query, token).map_err(|status| {
        Response::builder()
            .status(status)
            .body(axum::body::Body::from("Unauthorized: invalid or missing token"))
            .unwrap()
    })
}
