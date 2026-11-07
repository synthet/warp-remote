//! Shared-token authentication.
//!
//! The server is single-tenant: one secret token grants full remote control of every terminal
//! shared to it. Warp clients send it as `Authorization: Bearer <token>`; the web app exchanges it
//! once for an `HttpOnly` cookie so websockets (which cannot carry custom headers in browsers)
//! authenticate too.

use axum::http::HeaderMap;
use axum::http::header::{AUTHORIZATION, COOKIE, HOST, ORIGIN};

pub const COOKIE_NAME: &str = "synth_rc_token";

/// Compares in time independent of where the inputs first differ, so the token cannot be guessed
/// byte-by-byte from response timing.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == COOKIE_NAME).then_some(value)
        })
}

pub fn is_authorized(headers: &HeaderMap, token: &str) -> bool {
    [bearer_token(headers), cookie_token(headers)]
        .into_iter()
        .flatten()
        .any(|candidate| constant_time_eq(candidate.as_bytes(), token.as_bytes()))
}

/// Rejects cross-site websocket upgrades: a browser on another origin would otherwise ride the
/// viewer's cookie into a session. Native clients send no `Origin` and are allowed.
pub fn is_same_origin_or_native(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(HOST))
        .and_then(|v| v.to_str().ok());
    let origin_host = origin
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(origin);
    host.is_some_and(|host| host.eq_ignore_ascii_case(origin_host.trim_end_matches('/')))
}

/// Whether the client reached us over HTTPS, directly or through a TLS-terminating proxy.
pub fn is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|proto| proto.eq_ignore_ascii_case("https"))
}

pub fn login_cookie(token: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000{secure}")
}

pub fn logout_cookie() -> String {
    format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

/// A fresh random token for operators who did not configure one.
pub fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
