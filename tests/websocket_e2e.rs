//! Drives the real router over TCP: a sharer and a viewer connect with the protocol the Warp
//! client speaks, and unauthenticated or cross-origin upgrades are refused.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use remote_control_server::{AppState, Hub, HubConfig, router};
use session_sharing_protocol::common::{
    ActivePrompt, BlockId, FeatureSupport, InputReplicaId, OrderedTerminalEvent,
    OrderedTerminalEventType, Scrollback, Selection, UserID, WindowSize,
};
use session_sharing_protocol::{sharer, viewer};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const TOKEN: &str = "test-token-0123456789";

async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = AppState {
        hub: Arc::new(Hub::new(HubConfig::default())),
        token: TOKEN.into(),
        max_message_bytes: 16 * 1024 * 1024,
    };
    tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
    format!("127.0.0.1:{}", addr.port())
}

async fn connect(
    url: &str,
    headers: &[(&'static str, &str)],
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Error,
> {
    let mut request = url.into_client_request().unwrap();
    for (name, value) in headers {
        request.headers_mut().insert(*name, value.parse().unwrap());
    }
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(ws, _)| ws)
}

async fn recv_text<S>(ws: &mut S) -> String
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match ws.next().await.expect("stream open").expect("frame") {
            Message::Text(text) => return text.to_string(),
            Message::Close(_) => panic!("socket closed"),
            _ => continue,
        }
    }
}

fn init_payload() -> sharer::InitPayload {
    sharer::InitPayload {
        scrollback: Scrollback {
            blocks: vec![],
            is_alt_screen_active: false,
        },
        active_prompt: ActivePrompt::PS1,
        window_size: WindowSize {
            num_rows: 24,
            num_cols: 80,
        },
        user_id: UserID::default(),
        selection: Selection::None,
        init_block_id: BlockId::from("b0".to_owned()),
        input_replica_id: InputReplicaId::from("r0".to_owned()),
        telemetry_context: None,
        lifetime: sharer::Lifetime::Ephemeral,
        universal_developer_input_context: None,
        source_type: sharer::SessionSourceType::User,
        source_task_id: None,
        feature_support: FeatureSupport::default(),
        share_with_team_uid: None,
    }
}

#[tokio::test]
async fn sharer_and_viewer_exchange_messages_end_to_end() {
    let host = start_server().await;
    let bearer = format!("Bearer {TOKEN}");

    let mut sharer_ws = connect(
        &format!("ws://{host}/sessions/create?version=v0.test"),
        &[("authorization", &bearer), ("x-synth-host", "workstation")],
    )
    .await
    .expect("sharer connects");
    let init = sharer::UpstreamMessage::Initialize(init_payload())
        .to_json()
        .unwrap();
    sharer_ws.send(Message::Text(init.into())).await.unwrap();
    let initialized =
        sharer::DownstreamMessage::from_json(&recv_text(&mut sharer_ws).await).unwrap();
    let sharer::DownstreamMessage::SessionInitialized { session_id, .. } = initialized else {
        panic!("expected SessionInitialized");
    };

    let event = sharer::UpstreamMessage::OrderedTerminalEvent(OrderedTerminalEvent {
        event_no: 0,
        event_type: OrderedTerminalEventType::PtyBytesRead {
            bytes: b"$ echo hi\r\nhi\r\n".to_vec(),
        },
    });
    sharer_ws
        .send(Message::Text(event.to_json().unwrap().into()))
        .await
        .unwrap();

    let cookie = format!("synth_rc_token={TOKEN}");
    let origin = format!("http://{host}");
    let mut viewer_ws = connect(
        &format!("ws://{host}/sessions/join/{session_id}?client=web"),
        &[("cookie", &cookie), ("origin", &origin)],
    )
    .await
    .expect("viewer connects");
    let join = viewer::UpstreamMessage::Initialize(viewer::InitPayload {
        viewer_id: None,
        user_id: UserID::default(),
        last_received_event_no: None,
        latest_block_id: None,
        telemetry_context: None,
        feature_support: FeatureSupport::default(),
    });
    viewer_ws
        .send(Message::Text(join.to_json().unwrap().into()))
        .await
        .unwrap();

    let joined = viewer::DownstreamMessage::from_json(&recv_text(&mut viewer_ws).await).unwrap();
    assert!(matches!(
        joined,
        viewer::DownstreamMessage::JoinedSuccessfully {
            latest_event_no: Some(0),
            ..
        }
    ));
    let replayed = viewer::DownstreamMessage::from_json(&recv_text(&mut viewer_ws).await).unwrap();
    assert!(matches!(
        replayed,
        viewer::DownstreamMessage::OrderedTerminalEvent(OrderedTerminalEvent {
            event_type: OrderedTerminalEventType::PtyBytesRead { .. },
            ..
        })
    ));

    let sessions: serde_json::Value = {
        let response = tokio::task::spawn_blocking({
            let host = host.clone();
            move || {
                use std::io::{Read, Write};
                let mut stream = std::net::TcpStream::connect(&host).unwrap();
                write!(
                    stream,
                    "GET /api/sessions HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                let mut body = String::new();
                stream.read_to_string(&mut body).unwrap();
                body
            }
        })
        .await
        .unwrap();
        let json = response.split("\r\n\r\n").nth(1).unwrap();
        serde_json::from_str(json).unwrap()
    };
    assert_eq!(sessions[0]["id"], session_id.to_string());
    assert_eq!(sessions[0]["host"], "workstation");
    assert_eq!(sessions[0]["viewer_count"], 1);
}

#[tokio::test]
async fn upgrades_without_token_or_from_foreign_origins_are_refused() {
    let host = start_server().await;
    assert!(
        connect(&format!("ws://{host}/sessions/create"), &[])
            .await
            .is_err()
    );
    assert!(
        connect(
            &format!("ws://{host}/sessions/create"),
            &[("authorization", "Bearer wrong")],
        )
        .await
        .is_err()
    );
    let cookie = format!("synth_rc_token={TOKEN}");
    assert!(
        connect(
            &format!("ws://{host}/sessions/create"),
            &[("cookie", &cookie), ("origin", "https://evil.example")],
        )
        .await
        .is_err()
    );
}
