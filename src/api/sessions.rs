use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use sysinfo::{Disks, System};
use uuid::Uuid;

use crate::vm::{Capacity, CreateSessionRequest, SharedSessionManager};

#[derive(Deserialize)]
pub struct GithubTokenQuery {
    repo: String,
}

#[derive(serde::Serialize)]
pub struct HostMetrics {
    pub cpu_usage_percent: f32,
    pub load_average_one: f64,
    pub load_average_five: f64,
    pub load_average_fifteen: f64,
    pub total_memory_mb: u64,
    pub used_memory_mb: u64,
    pub available_memory_mb: u64,
    pub total_swap_mb: u64,
    pub used_swap_mb: u64,
    pub root_total_disk_mb: u64,
    pub root_available_disk_mb: u64,
    pub uptime_seconds: u64,
}

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

pub async fn get_host_metrics() -> impl IntoResponse {
    let mut system = System::new_all();
    system.refresh_memory();
    system.refresh_cpu_usage();
    let load_average = System::load_average();
    let uptime_seconds = System::uptime();

    let disks = Disks::new_with_refreshed_list();
    let root_disk = disks
        .iter()
        .find(|disk| disk.mount_point() == std::path::Path::new("/"));

    Json(HostMetrics {
        cpu_usage_percent: system.global_cpu_usage(),
        load_average_one: load_average.one,
        load_average_five: load_average.five,
        load_average_fifteen: load_average.fifteen,
        total_memory_mb: system.total_memory() / (1024 * 1024),
        used_memory_mb: system.used_memory() / (1024 * 1024),
        available_memory_mb: system.available_memory() / (1024 * 1024),
        total_swap_mb: system.total_swap() / (1024 * 1024),
        used_swap_mb: system.used_swap() / (1024 * 1024),
        root_total_disk_mb: root_disk.map(|disk| disk.total_space() / (1024 * 1024)).unwrap_or(0),
        root_available_disk_mb: root_disk
            .map(|disk| disk.available_space() / (1024 * 1024))
            .unwrap_or(0),
        uptime_seconds,
    })
}

pub async fn list_profiles(
    State(manager): State<SharedSessionManager>,
) -> impl IntoResponse {
    Json(manager.list_image_profiles())
}

pub async fn list_prompts() -> impl IntoResponse {
    Json(crate::prompts::built_in_prompts())
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

pub async fn github_token(
    State(manager): State<SharedSessionManager>,
    Path(id): Path<Uuid>,
    Query(query): Query<GithubTokenQuery>,
) -> impl IntoResponse {
    match manager.create_github_token_for_session(id, &query.repo).await {
        Ok(token) => (StatusCode::OK, token).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}
