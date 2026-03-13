use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use uuid::Uuid;

use crate::vm::{Capacity, CreateSessionRequest, SharedSessionManager};

pub async fn list_sessions(
    State(manager): State<SharedSessionManager>,
) -> impl IntoResponse {
    let sessions = manager.list_sessions().await;
    Json(sessions)
}

pub async fn get_session(
    State(manager): State<SharedSessionManager>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match manager.get_session(id).await {
        Some(s) => (StatusCode::OK, Json(s)).into_response(),
        None => (StatusCode::NOT_FOUND, "Session not found").into_response(),
    }
}

pub async fn get_capacity(
    State(manager): State<SharedSessionManager>,
) -> impl IntoResponse {
    let capacity: Capacity = manager.get_capacity().await;
    Json(capacity)
}

pub async fn list_profiles(
    State(manager): State<SharedSessionManager>,
) -> impl IntoResponse {
    Json(manager.list_image_profiles())
}

pub async fn create_session(
    State(manager): State<SharedSessionManager>,
    Json(req): Json<CreateSessionRequest>,
) -> impl IntoResponse {
    match manager.create_session(req).await {
        Ok(session) => (StatusCode::CREATED, Json(session)).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

pub async fn stop_session(
    State(manager): State<SharedSessionManager>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match manager.stop_session(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

pub async fn delete_session(
    State(manager): State<SharedSessionManager>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match manager.delete_session(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}
