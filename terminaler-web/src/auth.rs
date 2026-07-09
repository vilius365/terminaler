use axum::body::Body;
use axum::extract::Query;
use axum::http::header::{COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use std::collections::HashMap;

pub const AUTH_COOKIE_NAME: &str = "terminaler_web_auth";

pub enum PageAuthResult {
    Authorized,
    Redirect(Response),
}

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

fn parse_auth_cookie(headers: &HeaderMap) -> Option<&str> {
    let cookie_header = headers.get(COOKIE)?.to_str().ok()?;
    for cookie in cookie_header.split(';') {
        let mut parts = cookie.trim().splitn(2, '=');
        let name = parts.next()?.trim();
        let value = parts.next()?.trim();
        if name == AUTH_COOKIE_NAME {
            return Some(value);
        }
    }
    None
}

fn build_cookie(token: &str) -> String {
    format!("{AUTH_COOKIE_NAME}={token}; HttpOnly; SameSite=Strict; Path=/")
}

fn unauthorized_response() -> Response {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .body(Body::from("Unauthorized: invalid or missing token"))
        .unwrap()
}

pub fn authorize_page_request(
    headers: &HeaderMap,
    query: &Query<HashMap<String, String>>,
    expected: &str,
    redirect_path: &str,
) -> Result<PageAuthResult, Response> {
    if parse_auth_cookie(headers) == Some(expected) {
        return Ok(PageAuthResult::Authorized);
    }

    if validate_token(query, expected).is_ok() {
        return Ok(PageAuthResult::Redirect(
            Response::builder()
                .status(StatusCode::SEE_OTHER)
                .header(LOCATION, redirect_path)
                .header(SET_COOKIE, build_cookie(expected))
                .body(Body::empty())
                .unwrap(),
        ));
    }

    Err(unauthorized_response())
}

pub fn authorize_ws_request(headers: &HeaderMap, expected: &str) -> Result<(), Response> {
    if parse_auth_cookie(headers) == Some(expected) {
        Ok(())
    } else {
        Err(unauthorized_response())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_page_request_redirects_on_valid_bootstrap_token() {
        let headers = HeaderMap::new();
        let query = Query(HashMap::from([("token".to_string(), "secret".to_string())]));

        let result = authorize_page_request(&headers, &query, "secret", "/terminal").unwrap();
        let PageAuthResult::Redirect(response) = result else {
            panic!("expected redirect");
        };

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers().get(LOCATION).unwrap(), "/terminal");
        assert!(response
            .headers()
            .get(SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains(AUTH_COOKIE_NAME));
    }

    #[test]
    fn authorize_ws_request_accepts_matching_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, "foo=bar; terminaler_web_auth=secret".parse().unwrap());

        authorize_ws_request(&headers, "secret").unwrap();
    }

    #[test]
    fn authorize_ws_request_rejects_missing_cookie() {
        let headers = HeaderMap::new();

        assert!(authorize_ws_request(&headers, "secret").is_err());
    }
}
