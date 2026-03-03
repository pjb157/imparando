use subtle::ConstantTimeEq;
use axum::{
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::STANDARD, Engine};

#[derive(Clone)]
pub struct BasicAuthCredentials {
    pub user: String,
    pub pass: String,
}

pub async fn basic_auth_middleware(
    axum::extract::State(creds): axum::extract::State<BasicAuthCredentials>,
    req: Request,
    next: Next,
) -> Response {
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let authorized = auth_header
        .and_then(|h| h.strip_prefix("Basic "))
        .and_then(|encoded| STANDARD.decode(encoded).ok())
        .and_then(|decoded| String::from_utf8(decoded).ok())
        .map(|plain| {
            let mut parts = plain.splitn(2, ':');
            let user = parts.next().unwrap_or("");
            let pass = parts.next().unwrap_or("");
            // ct_eq returns subtle::Choice; & is constant-time AND; bool::from converts last.
            let user_ok = user.as_bytes().ct_eq(creds.user.as_bytes());
            let pass_ok = pass.as_bytes().ct_eq(creds.pass.as_bytes());
            bool::from(user_ok & pass_ok)
        })
        .unwrap_or(false);

    if authorized {
        next.run(req).await
    } else {
        // No WWW-Authenticate header — that header triggers the browser's
        // native credential dialog, which fights with the app's own login UI.
        (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
    }
}
