//! HTTP and websocket routes. Websocket paths match what the Warp client already dials for
//! session sharing (`/sessions/create`, `/sessions/{id}/resume`, `/sessions/join/{id}`), so
//! pointing `WARP_SESSION_SHARING_SERVER_URL` at this server is all a client needs.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use session_sharing_protocol::common::SessionId;
use session_sharing_protocol::{sharer, viewer};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::hub::{Hub, SharerLabels};
use crate::{auth, web};

/// Header a Synth Warp sharer uses to tell the dashboard which machine a terminal is on.
pub const HOST_LABEL_HEADER: &str = "x-synth-host";

#[derive(Clone)]
pub struct AppState {
    pub hub: Arc<Hub>,
    pub token: Arc<str>,
    pub max_message_bytes: usize,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/sessions/create", get(sharer_create))
        .route("/sessions/{session_id}/resume", get(sharer_resume))
        .route("/sessions/join/{session_id}", get(viewer_join))
        .route("/api/sessions", get(list_sessions))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/healthz", get(|| async { "ok" }))
        .route("/", get(web::index))
        .route("/session/{session_id}", get(web::index))
        .route("/assets/{*path}", get(web::asset))
        .route("/manifest.webmanifest", get(web::manifest))
        .route("/sw.js", get(web::service_worker))
        .with_state(state)
}

/// Periodically ends sessions whose sharer never came back.
pub fn spawn_reaper(hub: Arc<Hub>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            hub.reap_abandoned(Instant::now());
        }
    });
}

fn check_ws_auth(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    if !auth::is_same_origin_or_native(headers) {
        return Err(StatusCode::FORBIDDEN);
    }
    if !auth::is_authorized(headers, &state.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

fn configure(ws: WebSocketUpgrade, state: &AppState) -> WebSocketUpgrade {
    ws.max_message_size(state.max_message_bytes)
        .max_frame_size(state.max_message_bytes)
}

async fn sharer_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if let Err(status) = check_ws_auth(&state, &headers) {
        return status.into_response();
    }
    let labels = SharerLabels {
        host: headers
            .get(HOST_LABEL_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(|host| host.chars().take(128).collect()),
    };
    configure(ws, &state)
        .on_upgrade(move |socket| run_sharer(state.hub, socket, SharerHandshake::Create(labels)))
}

async fn sharer_resume(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if let Err(status) = check_ws_auth(&state, &headers) {
        return status.into_response();
    }
    let Ok(session_id) = session_id.parse::<SessionId>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    configure(ws, &state).on_upgrade(move |socket| {
        run_sharer(state.hub, socket, SharerHandshake::Resume(session_id))
    })
}

#[derive(Deserialize)]
struct JoinQuery {
    client: Option<String>,
}

async fn viewer_join(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Query(query): Query<JoinQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if let Err(status) = check_ws_auth(&state, &headers) {
        return status.into_response();
    }
    let Ok(session_id) = session_id.parse::<SessionId>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let display_name = match query.client.as_deref() {
        Some("web") => "Web remote".to_owned(),
        _ => "Warp viewer".to_owned(),
    };
    configure(ws, &state)
        .on_upgrade(move |socket| run_viewer(state.hub, socket, session_id, display_name))
}

async fn list_sessions(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !auth::is_authorized(&headers, &state.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    (
        [(CACHE_CONTROL, "no-store")],
        Json(state.hub.list_sessions()),
    )
        .into_response()
}

#[derive(Deserialize)]
struct LoginRequest {
    token: String,
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Response {
    if !auth::constant_time_eq(request.token.trim().as_bytes(), state.token.as_bytes()) {
        // Slows online guessing; the token itself carries ~244 bits when generated here.
        tokio::time::sleep(Duration::from_millis(750)).await;
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let cookie = auth::login_cookie(&state.token, auth::is_https(&headers));
    (StatusCode::NO_CONTENT, [(SET_COOKIE, cookie)]).into_response()
}

async fn logout() -> Response {
    (
        StatusCode::NO_CONTENT,
        [(SET_COOKIE, auth::logout_cookie())],
    )
        .into_response()
}

enum SharerHandshake {
    Create(SharerLabels),
    Resume(SessionId),
}

/// Forwards everything sent on the returned channel to the socket as JSON text frames. The
/// socket is closed once every sender is dropped, i.e. when the hub forgets the connection.
fn spawn_writer<T, F>(
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    to_json: F,
) -> mpsc::UnboundedSender<T>
where
    T: Send + 'static,
    F: Fn(&T) -> serde_json::Result<String> + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<T>();
    tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            match to_json(&message) {
                Ok(json) => {
                    if sink.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                Err(err) => log::error!("Failed to serialize downstream message: {err}"),
            }
        }
        let _ = sink.close().await;
    });
    tx
}

/// Reads the next protocol message, skipping control frames and malformed payloads.
async fn next_message<T: DeserializeOwned>(stream: &mut SplitStream<WebSocket>) -> Option<T> {
    while let Some(frame) = stream.next().await {
        let parsed = match frame.ok()? {
            Message::Text(text) => serde_json::from_str(text.as_str()),
            Message::Binary(bytes) => serde_json::from_slice(&bytes),
            Message::Close(_) => return None,
            Message::Ping(_) | Message::Pong(_) => continue,
        };
        match parsed {
            Ok(message) => return Some(message),
            Err(err) => log::warn!("Dropping unparseable websocket message: {err}"),
        }
    }
    None
}

async fn run_sharer(hub: Arc<Hub>, socket: WebSocket, handshake: SharerHandshake) {
    let (sink, mut stream) = socket.split();
    let tx = spawn_writer(sink, sharer::DownstreamMessage::to_json);
    let Some(first) = next_message::<sharer::UpstreamMessage>(&mut stream).await else {
        return;
    };

    let established = match (handshake, first) {
        (SharerHandshake::Create(labels), sharer::UpstreamMessage::Initialize(init)) => {
            hub.create_session(init, labels, tx)
        }
        (SharerHandshake::Resume(session_id), sharer::UpstreamMessage::Reconnect(payload)) => hub
            .resume_session(session_id, payload, tx)
            .map(|conn_id| (session_id, conn_id)),
        _ => {
            log::warn!("Sharer sent an unexpected handshake message");
            None
        }
    };
    let Some((session_id, conn_id)) = established else {
        return;
    };

    while let Some(message) = next_message(&mut stream).await {
        hub.handle_sharer_message(session_id, conn_id, message);
    }
    hub.sharer_disconnected(session_id, conn_id);
}

async fn run_viewer(hub: Arc<Hub>, socket: WebSocket, session_id: SessionId, name: String) {
    let (sink, mut stream) = socket.split();
    let tx = spawn_writer(sink, viewer::DownstreamMessage::to_json);
    let Some(first) = next_message::<viewer::UpstreamMessage>(&mut stream).await else {
        return;
    };
    let viewer::UpstreamMessage::Initialize(init) = first else {
        log::warn!("Viewer sent an unexpected handshake message");
        return;
    };
    let Some((viewer_id, conn_id)) = hub.join_session(session_id, init, name, tx) else {
        return;
    };

    while let Some(message) = next_message(&mut stream).await {
        hub.handle_viewer_message(session_id, &viewer_id, conn_id, message);
    }
    hub.viewer_disconnected(session_id, &viewer_id, conn_id);
}

/// Returns `(content-type, body)` responses with no caching for HTML entry points.
pub(crate) fn static_response(
    content_type: &'static str,
    body: &'static [u8],
    cache: &'static str,
) -> Response {
    ([(CONTENT_TYPE, content_type), (CACHE_CONTROL, cache)], body).into_response()
}
