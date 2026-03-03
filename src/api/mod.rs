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

    // API routes are protected; the UI route is open so the HTML loads first
    // and shows its own login overlay (browser native Basic Auth dialog avoided).
    let api = Router::new()
        .route("/api/sessions", get(sessions::list_sessions))
        .route("/api/sessions", post(sessions::create_session))
        .route("/api/sessions/:id", get(sessions::get_session))
        .route("/api/sessions/:id/stop", put(sessions::stop_session))
        .route("/api/sessions/:id", delete(sessions::delete_session))
        .route("/api/sessions/:id/terminal", get(terminal::terminal_ws))
        .with_state(manager)
        .layer(middleware::from_fn_with_state(creds, basic_auth_middleware));

    Router::new().merge(api).route("/", get(serve_ui))
}

async fn serve_ui() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("../ui/index.html"))
}
