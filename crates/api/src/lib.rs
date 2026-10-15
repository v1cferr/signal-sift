//! HTTP/JSON API.
//!
//! Contracts are client-neutral: nothing here knows about QML or GLANCE. Every
//! versioned route lives under `/v1`; `/health` stays unversioned so that
//! service managers can probe it without caring about API evolution.

mod error;
mod proc_stats;
pub mod views;

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use signalsift_core::Profile;
use signalsift_storage::{Db, Summary};
use tokio::sync::watch;

pub use error::ApiError;
pub use proc_stats::{ProcStats, read_self_stats};

#[derive(Clone)]
pub struct AppState {
    inner: Arc<StateInner>,
}

struct StateInner {
    db: Db,
    profile: watch::Receiver<Profile>,
    started: Instant,
}

impl AppState {
    pub fn new(db: Db, profile: watch::Receiver<Profile>) -> Self {
        Self {
            inner: Arc::new(StateInner {
                db,
                profile,
                started: Instant::now(),
            }),
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/status", get(status))
        .route("/v1/items", get(list_items))
        .route("/v1/items/{id}", get(get_item))
        .route("/v1/items/{id}/segments", get(get_segments))
        .fallback(|| async { ApiError::NotFound("no such route".into()) })
        .with_state(state)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

/// Liveness only: no database access, so it stays cheap enough to poll.
async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub api_version: &'static str,
    pub version: &'static str,
    pub uptime_secs: u64,
    pub profile: Profile,
    pub database: Summary,
    /// Resident memory and CPU time of the daemon, read from /proc. Absent on
    /// platforms without procfs.
    pub process: Option<ProcStats>,
}

async fn status(State(state): State<AppState>) -> Result<Json<StatusResponse>, ApiError> {
    let database = state.inner.db.summary().await?;
    Ok(Json(StatusResponse {
        api_version: "v1",
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: state.inner.started.elapsed().as_secs(),
        profile: *state.inner.profile.borrow(),
        database,
        process: read_self_stats(),
    }))
}

#[derive(Deserialize)]
struct PageQuery {
    limit: Option<u32>,
    cursor: Option<String>,
}

async fn list_items(
    State(state): State<AppState>,
    Query(q): Query<PageQuery>,
) -> Result<Json<views::Page<views::ItemSummary>>, ApiError> {
    let cursor = match q.cursor {
        Some(c) => Some(
            c.parse::<i64>()
                .map_err(|_| ApiError::BadRequest("invalid cursor".into()))?,
        ),
        None => None,
    };
    let limit = q.limit.unwrap_or(20);
    let page = state
        .inner
        .db
        .read(move |c| views::list_items(c, limit, cursor))
        .await?;
    Ok(Json(page))
}

async fn get_item(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<views::ItemDetail>, ApiError> {
    state
        .inner
        .db
        .read(move |c| views::item_detail(c, id))
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("item {id} not found")))
}

async fn get_segments(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<SegmentsResponse>, ApiError> {
    let segments = state
        .inner
        .db
        .read(move |c| views::item_segments(c, id))
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("item {id} not found")))?;
    Ok(Json(SegmentsResponse {
        item_id: id,
        segments,
    }))
}

#[derive(Serialize)]
struct SegmentsResponse {
    item_id: i64,
    segments: Vec<views::SegmentView>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let (_tx, rx) = watch::channel(Profile::Gaming);
        (router(AppState::new(db, rx)), dir)
    }

    async fn get_json(app: Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let res = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn health_is_ok() {
        let (app, _dir) = app();
        let (code, body) = get_json(app, "/health").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn status_reports_profile_and_schema() {
        let (app, _dir) = app();
        let (code, body) = get_json(app, "/v1/status").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["profile"], "gaming");
        assert_eq!(body["database"]["schema_version"], 1);
    }

    #[tokio::test]
    async fn items_and_segments_round_trip_over_http() {
        let (db, _dir, id) = views::tests::seeded().await;
        let (_tx, rx) = watch::channel(Profile::Balanced);
        let app = router(AppState::new(db, rx));
        let (code, page) = get_json(app.clone(), "/v1/items?limit=1").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(page["items"][0]["id"], id);
        assert_eq!(page["items"][0]["transcript"]["partial"], true);
        let (code, segs) = get_json(app.clone(), &format!("/v1/items/{id}/segments")).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(segs["segments"][0]["start"], "12:34");
        let (code, body) = get_json(app.clone(), "/v1/items/999").await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
        let (code, _) = get_json(app, "/v1/items?cursor=abc").await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
    }

    /// Paths documented in contracts/openapi.yaml, read without a YAML
    /// parser: path keys are the two-space-indented lines under `paths:`.
    fn documented_paths() -> Vec<String> {
        let spec = include_str!("../../../contracts/openapi.yaml");
        spec.lines()
            .skip_while(|l| *l != "paths:")
            .skip(1)
            .take_while(|l| l.is_empty() || l.starts_with(' '))
            .filter_map(|l| l.strip_prefix("  /").and_then(|p| p.strip_suffix(':')))
            .map(|p| format!("/{p}"))
            .collect()
    }

    #[tokio::test]
    async fn every_documented_path_is_routed() {
        let paths = documented_paths();
        assert!(paths.len() >= 5, "parser found {paths:?}");
        for path in paths {
            let (app, _dir) = app();
            let concrete = path.replace("{id}", "1");
            let res = app
                .oneshot(Request::get(&concrete).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            // A missing item is fine (404 not_found from the handler); a
            // missing route would also be 404, but with the router's message.
            assert_ne!(
                body["error"]["message"], "no such route",
                "{path} is documented but not routed"
            );
        }
    }

    #[test]
    fn every_route_is_documented() {
        let documented = documented_paths();
        let source = include_str!("lib.rs");
        let routes: Vec<&str> = source
            .lines()
            .filter(|l| l.trim_start().starts_with(".route(\""))
            .collect();
        assert!(routes.len() >= 5, "found only {} routes", routes.len());
        for line in routes {
            let path = line.split('"').nth(1).unwrap();
            assert!(
                documented.iter().any(|d| d == path),
                "{path} is routed but missing from openapi.yaml"
            );
        }
    }

    #[tokio::test]
    async fn unknown_routes_use_the_error_envelope() {
        let (app, _dir) = app();
        let (code, body) = get_json(app, "/v1/nope").await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
    }
}
