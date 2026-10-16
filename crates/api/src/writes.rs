//! Write endpoints. All of them sit behind the bearer-token middleware.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use signalsift_core::Profile;
use signalsift_core::youtube;
use signalsift_storage::feedback::{self, NewFeedback};
use signalsift_storage::jobs::{self, MEDIA_ASR, MEDIA_CAPTIONS, WorkClass};
use signalsift_storage::{StorageError, now_ms};

use crate::{ApiError, AppState};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileBody {
    profile: Profile,
}

#[derive(Serialize)]
pub struct ProfileResponse {
    profile: Profile,
    previous: Profile,
}

/// Switches the execution profile immediately. Workers observe the change:
/// switching to `gaming` or `eco` cancels a running transcription, which
/// resumes from its checkpoint when heavy work is allowed again. Not
/// persisted: a restart returns to the configured profile.
pub async fn set_profile(
    State(state): State<AppState>,
    body: Result<Json<ProfileBody>, JsonRejection>,
) -> Result<Json<ProfileResponse>, ApiError> {
    let Json(body) = body?;
    let previous = state.inner.profile.send_replace(body.profile);
    tracing::info!(%previous, profile = %body.profile, "profile changed through the API");
    Ok(Json(ProfileResponse {
        profile: body.profile,
        previous,
    }))
}

const MAX_NOTE_CHARS: usize = 2000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackBody {
    item_id: i64,
    segment_id: Option<i64>,
    /// `interested`, `not_interested`, `already_known` or `wrong_topic`.
    verdict: String,
    note: Option<String>,
    /// Free-form client name, e.g. `glance`.
    client: Option<String>,
}

#[derive(Serialize)]
pub struct Created {
    id: i64,
}

pub async fn post_feedback(
    State(state): State<AppState>,
    body: Result<Json<FeedbackBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    let Json(body) = body?;
    if !feedback::VERDICTS.contains(&body.verdict.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "verdict must be one of {}",
            feedback::VERDICTS.join(", ")
        )));
    }
    if body
        .note
        .as_ref()
        .is_some_and(|n| n.chars().count() > MAX_NOTE_CHARS)
    {
        return Err(ApiError::BadRequest(format!(
            "note exceeds {MAX_NOTE_CHARS} characters"
        )));
    }
    let f = NewFeedback {
        item_id: body.item_id,
        segment_id: body.segment_id,
        verdict: body.verdict,
        note: body.note,
        client: body.client.map(|c| c.chars().take(64).collect()),
    };
    let id = state
        .inner
        .db
        .write(move |c| feedback::insert(c, &f))
        .await
        .map_err(|e| match e {
            StorageError::Invalid(m) => ApiError::BadRequest(m),
            other => other.into(),
        })?
        .ok_or_else(|| ApiError::NotFound("item or segment not found".into()))?;
    Ok((StatusCode::CREATED, Json(Created { id })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaBody {
    /// YouTube URL or video id.
    url: String,
    /// `auto` (captions, transcribing only if none) or `force` (transcribe).
    #[serde(default)]
    transcribe: Transcribe,
}

#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Transcribe {
    #[default]
    Auto,
    Force,
}

#[derive(Serialize)]
pub struct Queued {
    video_id: String,
    job: &'static str,
    /// False when the same work was already queued or done.
    queued: bool,
}

pub async fn queue_media(
    State(state): State<AppState>,
    body: Result<Json<MediaBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Queued>), ApiError> {
    let Json(body) = body?;
    let video_id = youtube::video_id(&body.url)
        .ok_or_else(|| ApiError::BadRequest("not a YouTube video URL or id".into()))?;
    let (kind, class, prefix) = match body.transcribe {
        Transcribe::Auto => (MEDIA_CAPTIONS, WorkClass::Light, "captions"),
        Transcribe::Force => (MEDIA_ASR, WorkClass::Heavy, "asr"),
    };
    let payload = serde_json::json!({ "video_id": video_id }).to_string();
    let key = format!("{prefix}:{video_id}");
    let queued = state
        .inner
        .db
        .write(move |c| jobs::enqueue(c, kind, &key, class, &payload, now_ms()))
        .await?
        .is_some();
    if queued {
        // Wake the worker of the job's own class; a heavy worker may be
        // sleeping for minutes and would not see a light-worker wakeup.
        match class {
            WorkClass::Light => state.inner.light_ready.notify_one(),
            WorkClass::Heavy => state.inner.heavy_ready.notify_one(),
        }
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(Queued {
            video_id,
            job: kind,
            queued,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use signalsift_core::Profile;
    use tower::ServiceExt;

    use crate::tests::{TEST_TOKEN, test_state};
    use crate::{router, views};

    async fn send(
        app: axum::Router,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: &str,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let res = app
            .oneshot(req.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn writes_require_the_token() {
        let (db, _dir, _) = views::tests::seeded().await;
        let app = router(test_state(db, Profile::Balanced));
        let body = r#"{"profile":"gaming"}"#;
        let (code, err) = send(app.clone(), "PUT", "/v1/profile", None, body).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);
        assert_eq!(err["error"]["code"], "unauthorized");
        let (code, _) = send(app.clone(), "PUT", "/v1/profile", Some("wrong"), body).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);
        let (code, res) = send(app.clone(), "PUT", "/v1/profile", Some(TEST_TOKEN), body).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(
            (res["profile"].as_str(), res["previous"].as_str()),
            (Some("gaming"), Some("balanced"))
        );
        let (_, status) = send(app, "GET", "/v1/status", None, "").await;
        assert_eq!(status["profile"], "gaming");
    }

    #[tokio::test]
    async fn feedback_is_validated_and_stored() {
        let (db, _dir, item) = views::tests::seeded().await;
        let app = router(test_state(db, Profile::Balanced));
        let ok = format!(r#"{{"item_id":{item},"verdict":"interested","client":"glance"}}"#);
        let (code, created) =
            send(app.clone(), "POST", "/v1/feedback", Some(TEST_TOKEN), &ok).await;
        assert_eq!(code, StatusCode::CREATED);
        assert!(created["id"].as_i64().is_some());
        let bad = format!(r#"{{"item_id":{item},"verdict":"love it"}}"#);
        let (code, _) = send(app.clone(), "POST", "/v1/feedback", Some(TEST_TOKEN), &bad).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        let missing = r#"{"item_id":999,"verdict":"interested"}"#;
        let (code, _) = send(app, "POST", "/v1/feedback", Some(TEST_TOKEN), missing).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn media_queueing_is_idempotent_and_validates_input() {
        let (db, _dir, _) = views::tests::seeded().await;
        let app = router(test_state(db, Profile::Balanced));
        let body = r#"{"url":"https://youtu.be/N3eU-2VIByQ"}"#;
        let (code, first) = send(app.clone(), "POST", "/v1/media", Some(TEST_TOKEN), body).await;
        assert_eq!(code, StatusCode::ACCEPTED);
        assert_eq!(first["queued"], true);
        let (_, second) = send(app.clone(), "POST", "/v1/media", Some(TEST_TOKEN), body).await;
        assert_eq!(second["queued"], false);
        let evil = r#"{"url":"http://169.254.169.254/latest"}"#;
        let (code, _) = send(app, "POST", "/v1/media", Some(TEST_TOKEN), evil).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn oversized_bodies_are_refused() {
        let (db, _dir, _) = views::tests::seeded().await;
        let app = router(test_state(db, Profile::Balanced));
        let note = "x".repeat(20 * 1024);
        let body = format!(r#"{{"item_id":1,"verdict":"interested","note":"{note}"}}"#);
        let (code, err) = send(app.clone(), "POST", "/v1/feedback", Some(TEST_TOKEN), &body).await;
        assert_eq!(code, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(err["error"]["code"], "payload_too_large");
        let (code, err) = send(app, "POST", "/v1/feedback", Some(TEST_TOKEN), "{not json").await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(err["error"]["code"], "bad_request");
    }
}
