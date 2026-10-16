//! Bearer-token protection for write endpoints.
//!
//! Even on loopback, any local process, and any web page through the browser,
//! can send requests to 127.0.0.1. Reads expose only public content, so they
//! stay open; anything that changes state requires the token. The token lives
//! in the state directory with mode 0600, so only the user's own trusted
//! clients (the CLI, GLANCE's fetch script) can read it.

use std::io::{Read, Write};
use std::path::Path;

use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::{ApiError, AppState};

pub const TOKEN_FILE: &str = "api-token";

/// Reads the token from `dir/api-token`, creating it (32 random bytes, hex)
/// with mode 0600 on first use.
pub fn ensure_token(dir: &Path) -> std::io::Result<String> {
    let path = dir.join(TOKEN_FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let t = existing.trim();
        if t.len() >= 32 {
            return Ok(t.to_owned());
        }
    }
    std::fs::create_dir_all(dir)?;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(token.as_bytes())?;
    Ok(token)
}

pub fn read_token(dir: &Path) -> std::io::Result<String> {
    Ok(std::fs::read_to_string(dir.join(TOKEN_FILE))?
        .trim()
        .to_owned())
}

pub async fn require_token(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD) {
        return next.run(req).await;
    }
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match presented {
        Some(t) if constant_time_eq(t.as_bytes(), state.token().as_bytes()) => next.run(req).await,
        _ => ApiError::Unauthorized.into_response(),
    }
}

/// Comparison whose duration does not depend on where the inputs differ.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_created_once_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let a = ensure_token(dir.path()).unwrap();
        let b = ensure_token(dir.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(TOKEN_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn comparison_requires_exact_match() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
