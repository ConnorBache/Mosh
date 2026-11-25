use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use mosh_core::Mosh;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};
use subtle::ConstantTimeEq;

struct Service {
    db: PathBuf,
    actor: String,
    token: Vec<u8>,
    slots: tokio::sync::Semaphore,
}

async fn request(
    State(state): State<Arc<Service>>,
    Path(command): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if !auth.is_some_and(|token| bool::from(token.as_bytes().ct_eq(&state.token))) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"invalid bearer token"})),
        )
            .into_response();
    }
    if !matches!(
        command.as_str(),
        "roots"
            | "search"
            | "inspect"
            | "preview"
            | "history"
            | "tag_catalog"
            | "tag_coverage"
            | "graph"
            | "saved"
            | "tier"
            | "individuality"
            | "vocabulary_advice"
            | "passages"
            | "ask"
            | "feedback"
            | "mosh_info"
            | "mosh_route"
            | "mosh_handoff"
    ) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"unknown read endpoint"})),
        )
            .into_response();
    }
    let Ok(_slot) = state.slots.try_acquire() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"Ledger is busy"})),
        )
            .into_response();
    };
    let backend = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        Mosh::as_actor(&backend.db, &backend.actor)?.command(
            &command,
            &payload,
            &AtomicBool::new(false),
        )
    })
    .await;
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(_)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"query or object is unavailable"})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"service worker failed"})),
        )
            .into_response(),
    }
}

#[tokio::main]
pub async fn serve(db: PathBuf, port: u16, actor: String) -> Result<()> {
    let token = std::env::var("MOSH_API_TOKEN")
        .context("set MOSH_API_TOKEN to a random bearer token of at least 32 characters")?;
    ensure!(
        token.len() >= 32 && token.is_ascii() && token.bytes().all(|c| c.is_ascii_graphic()),
        "invalid MOSH_API_TOKEN"
    );
    Mosh::as_actor(&db, &actor)?;
    let state = Arc::new(Service {
        db,
        actor,
        token: token.into_bytes(),
        slots: tokio::sync::Semaphore::new(4),
    });
    let app = Router::new()
        .route("/v1/{command}", post(request))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    eprintln!("Mosh local API listening on {}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}
