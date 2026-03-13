pub mod auth;
pub mod sessions;
pub mod terminal;

use axum::{
    middleware,
    routing::{delete, get, post, put},
    Router,
};

use crate::vm::SharedSessionManager;
use auth::{basic_auth_middleware, BasicAuthCredentials};

pub fn router(manager: SharedSessionManager, user: String, pass: String) -> Router {
    let creds = BasicAuthCredentials { user, pass };

    // Session management routes require authentication.
    let api = Router::new()
        .route("/api/sessions", get(sessions::list_sessions))
        .route("/api/capacity", get(sessions::get_capacity))
        .route("/api/profiles", get(sessions::list_profiles))
        .route("/api/sessions", post(sessions::create_session))
        .route("/api/sessions/:id", get(sessions::get_session))
        .route("/api/sessions/:id/stop", put(sessions::stop_session))
        .route("/api/sessions/:id", delete(sessions::delete_session))
        .with_state(manager.clone())
        .layer(middleware::from_fn_with_state(creds, basic_auth_middleware));

    // Terminal WebSocket is outside auth — the session UUID acts as a capability
    // token. (Browser WS APIs can't send arbitrary headers, making Basic Auth
    // in the URL unreliable across browsers.)
    let terminal = Router::new()
        .route("/api/sessions/:id/terminal", get(terminal::terminal_ws))
        .with_state(manager);

    Router::new().merge(api).merge(terminal).route("/", get(serve_ui))
}

async fn serve_ui() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("../ui/index.html"))
}
