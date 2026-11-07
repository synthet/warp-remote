use std::time::Duration;

use session_sharing_protocol::common::{
    ActivePrompt, BlockId, BufferId, CommandExecutionFailureReason, FeatureSupport, InputReplicaId,
    OrderedTerminalEvent, OrderedTerminalEventType, ParticipantId, Role, Scrollback,
    ScrollbackBlock, Selection, UserID, WindowSize, WriteToPtyRequestId, WriteToPtySeqNo,
};
use session_sharing_protocol::sharer::{
    self, Lifetime, ReconnectPayload, ReconnectToken, ReconnectionFailedReason, SessionSourceType,
};
use session_sharing_protocol::viewer;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::time::Instant;

use super::*;

fn drain<T>(rx: &mut UnboundedReceiver<T>) -> Vec<T> {
    let mut out = Vec::new();
    while let Ok(message) = rx.try_recv() {
        out.push(message);
    }
    out
}

fn sharer_init(scrollback_bytes: usize) -> sharer::InitPayload {
    let block =
        serde_json::json!({ "pwd": "/home/me/project", "filler": "x".repeat(scrollback_bytes) });
    sharer::InitPayload {
        scrollback: Scrollback {
            blocks: vec![ScrollbackBlock {
                raw: serde_json::to_vec(&block).unwrap(),
            }],
            is_alt_screen_active: false,
        },
        active_prompt: ActivePrompt::PS1,
        window_size: WindowSize {
            num_rows: 40,
            num_cols: 120,
        },
        user_id: UserID::default(),
        selection: Selection::None,
        init_block_id: BlockId::from("block-0".to_owned()),
        input_replica_id: InputReplicaId::from("sharer-replica".to_owned()),
        telemetry_context: None,
        lifetime: Lifetime::Ephemeral,
        universal_developer_input_context: None,
        source_type: SessionSourceType::User,
        source_task_id: None,
        feature_support: FeatureSupport::default(),
        share_with_team_uid: None,
    }
}

fn viewer_init(viewer_id: Option<ParticipantId>, last: Option<usize>) -> viewer::InitPayload {
    viewer::InitPayload {
        viewer_id,
        user_id: UserID::default(),
        last_received_event_no: last,
        latest_block_id: None,
        telemetry_context: None,
        feature_support: FeatureSupport {
            supports_agent_view: false,
            supports_full_role: true,
            supports_full_role_for_real: true,
        },
    }
}

fn pty_event(event_no: usize, bytes: &[u8]) -> sharer::UpstreamMessage {
    sharer::UpstreamMessage::OrderedTerminalEvent(OrderedTerminalEvent {
        event_no,
        event_type: OrderedTerminalEventType::PtyBytesRead {
            bytes: bytes.to_vec(),
        },
    })
}

struct Fixture {
    hub: Hub,
    session_id: SessionId,
    sharer_conn: ConnId,
    sharer_rx: UnboundedReceiver<sharer::DownstreamMessage>,
    secret: SessionSecret,
    reconnect_token: ReconnectToken,
}

fn fixture(config: HubConfig) -> Fixture {
    let hub = Hub::new(config);
    let (tx, mut sharer_rx) = mpsc::unbounded_channel();
    let (session_id, sharer_conn) = hub
        .create_session(
            sharer_init(16),
            SharerLabels {
                host: Some("laptop".to_owned()),
            },
            tx,
        )
        .expect("session created");
    let Some(sharer::DownstreamMessage::SessionInitialized {
        session_id: initialized_id,
        session_secret,
        reconnect_token,
        sharer_firebase_uid,
        ..
    }) = drain(&mut sharer_rx).into_iter().next()
    else {
        panic!("expected SessionInitialized");
    };
    assert_eq!(initialized_id, session_id);
    assert_eq!(sharer_firebase_uid, "synth-owner");
    Fixture {
        hub,
        session_id,
        sharer_conn,
        sharer_rx,
        secret: session_secret,
        reconnect_token,
    }
}

fn join(
    f: &Fixture,
    init: viewer::InitPayload,
) -> (
    ParticipantId,
    ConnId,
    UnboundedReceiver<viewer::DownstreamMessage>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (viewer_id, conn_id) = f
        .hub
        .join_session(f.session_id, init, "Web remote".to_owned(), tx)
        .expect("joined");
    (viewer_id, conn_id, rx)
}

#[test]
fn viewer_join_receives_scrollback_then_replayed_events() {
    let mut f = fixture(HubConfig::default());
    f.hub
        .handle_sharer_message(f.session_id, f.sharer_conn, pty_event(0, b"hello"));
    f.hub
        .handle_sharer_message(f.session_id, f.sharer_conn, pty_event(1, b" world"));
    let acks = drain(&mut f.sharer_rx);
    assert!(matches!(
        acks.last(),
        Some(sharer::DownstreamMessage::EventsProcessedAck {
            latest_processed_event_no: 1
        })
    ));

    let (viewer_id, _, mut rx) = join(&f, viewer_init(None, None));
    let messages = drain(&mut rx);
    let viewer::DownstreamMessage::JoinedSuccessfully {
        scrollback,
        latest_event_no,
        viewer_id: joined_id,
        viewer_firebase_uid,
        participant_list,
        ..
    } = &messages[0]
    else {
        panic!("first message must be JoinedSuccessfully");
    };
    assert_eq!(scrollback.blocks.len(), 1);
    assert_eq!(*latest_event_no, Some(1));
    assert_eq!(*joined_id, viewer_id);
    assert_eq!(viewer_firebase_uid, "synth-owner");
    assert_eq!(participant_list.viewers[0].role, Role::Full);

    let replayed: Vec<usize> = messages
        .iter()
        .filter_map(|m| match m {
            viewer::DownstreamMessage::OrderedTerminalEvent(e) => Some(e.event_no),
            _ => None,
        })
        .collect();
    assert_eq!(replayed, vec![0, 1]);

    // The sharer learns about the new viewer, with a role that lets it drive the terminal.
    let sharer_messages = drain(&mut f.sharer_rx);
    assert!(sharer_messages.iter().any(|m| matches!(
        m,
        sharer::DownstreamMessage::ParticipantListUpdated(list)
            if list.present_viewers.len() == 1 && list.viewers[0].role.can_execute()
    )));
}

#[test]
fn duplicate_events_after_resume_are_acked_but_not_stored() {
    let mut f = fixture(HubConfig::default());
    f.hub
        .handle_sharer_message(f.session_id, f.sharer_conn, pty_event(0, b"a"));
    f.hub
        .handle_sharer_message(f.session_id, f.sharer_conn, pty_event(0, b"a"));
    drain(&mut f.sharer_rx);

    let (_, _, mut rx) = join(&f, viewer_init(None, None));
    let events = drain(&mut rx)
        .into_iter()
        .filter(|m| matches!(m, viewer::DownstreamMessage::OrderedTerminalEvent(_)))
        .count();
    assert_eq!(events, 1);
}

#[test]
fn write_to_pty_is_relayed_under_the_senders_identity() {
    let mut f = fixture(HubConfig::default());
    let (viewer_id, conn, _rx) = join(&f, viewer_init(None, None));
    drain(&mut f.sharer_rx);

    let spoofed = WriteToPtyRequestId {
        participant_id: ParticipantId::from("someone-else".to_owned()),
        op_no: WriteToPtySeqNo::zero(),
    };
    f.hub.handle_viewer_message(
        f.session_id,
        &viewer_id,
        conn,
        viewer::UpstreamMessage::WriteToPty {
            request_id: spoofed,
            bytes: b"y\r".to_vec(),
        },
    );
    let messages = drain(&mut f.sharer_rx);
    assert!(matches!(
        messages.as_slice(),
        [sharer::DownstreamMessage::WriteToPtyRequested { id, bytes }]
            if id.participant_id == viewer_id && bytes == b"y\r"
    ));
}

#[test]
fn reader_viewers_cannot_execute() {
    let mut f = fixture(HubConfig {
        viewer_role: Role::Reader,
        ..Default::default()
    });
    let (viewer_id, conn, mut rx) = join(&f, viewer_init(None, None));
    drain(&mut rx);
    drain(&mut f.sharer_rx);

    f.hub.handle_viewer_message(
        f.session_id,
        &viewer_id,
        conn,
        viewer::UpstreamMessage::ExecuteCommand {
            buffer_id: BufferId::from("block-0".to_owned()),
            command: "rm -rf /".to_owned(),
        },
    );
    assert!(drain(&mut f.sharer_rx).is_empty());
    assert!(matches!(
        drain(&mut rx).as_slice(),
        [viewer::DownstreamMessage::CommandExecutionRequestFailed {
            reason: CommandExecutionFailureReason::InsufficientPermissions,
            ..
        }]
    ));
}

#[test]
fn execute_command_is_relayed_to_the_sharer() {
    let mut f = fixture(HubConfig::default());
    let (viewer_id, conn, mut rx) = join(&f, viewer_init(None, None));
    drain(&mut rx);
    drain(&mut f.sharer_rx);

    f.hub.handle_viewer_message(
        f.session_id,
        &viewer_id,
        conn,
        viewer::UpstreamMessage::ExecuteCommand {
            buffer_id: BufferId::from("block-0".to_owned()),
            command: "ls".to_owned(),
        },
    );
    assert!(matches!(
        drain(&mut rx).as_slice(),
        [viewer::DownstreamMessage::CommandExecutionRequestInFlight(
            _
        )]
    ));
    assert!(matches!(
        drain(&mut f.sharer_rx).as_slice(),
        [sharer::DownstreamMessage::CommandExecutionRequested { participant_id, command, .. }]
            if *participant_id == viewer_id && command == "ls"
    ));
}

#[test]
fn viewer_terminal_size_reaches_the_sharer() {
    let mut f = fixture(HubConfig::default());
    let (viewer_id, conn, _rx) = join(&f, viewer_init(None, None));
    drain(&mut f.sharer_rx);

    f.hub.handle_viewer_message(
        f.session_id,
        &viewer_id,
        conn,
        viewer::UpstreamMessage::ReportTerminalSize {
            window_size: WindowSize {
                num_rows: 30,
                num_cols: 48,
            },
        },
    );
    assert!(matches!(
        drain(&mut f.sharer_rx).as_slice(),
        [sharer::DownstreamMessage::ViewerTerminalSizeReported { window_size, .. }]
            if window_size.num_cols == 48
    ));
}

#[test]
fn resume_requires_secret_and_token_and_reports_last_event() {
    let mut f = fixture(HubConfig::default());
    f.hub
        .handle_sharer_message(f.session_id, f.sharer_conn, pty_event(0, b"a"));
    f.hub.sharer_disconnected(f.session_id, f.sharer_conn);
    drain(&mut f.sharer_rx);

    let payload = |reconnect_token: ReconnectToken| ReconnectPayload {
        session_secret: f.secret.clone(),
        reconnect_token,
        user_id: UserID::default(),
        latest_block_id: BlockId::from("block-0".to_owned()),
        selection: Selection::None,
        feature_support: FeatureSupport::default(),
    };

    let (tx, mut rx) = mpsc::unbounded_channel();
    assert!(
        f.hub
            .resume_session(f.session_id, payload(ReconnectToken::new()), tx)
            .is_none()
    );
    assert!(matches!(
        drain(&mut rx).as_slice(),
        [sharer::DownstreamMessage::FailedToReconnect {
            reason: ReconnectionFailedReason::WrongReconnectionToken
        }]
    ));

    let (tx, mut rx) = mpsc::unbounded_channel();
    let conn = f
        .hub
        .resume_session(f.session_id, payload(f.reconnect_token.clone()), tx)
        .expect("resumed");
    assert!(matches!(
        drain(&mut rx).as_slice(),
        [sharer::DownstreamMessage::SessionReconnected {
            last_received_event_no: Some(0),
            ..
        }]
    ));

    // The pre-resume connection id is stale and must be ignored.
    f.hub
        .handle_sharer_message(f.session_id, f.sharer_conn, pty_event(1, b"stale"));
    f.hub
        .handle_sharer_message(f.session_id, conn, pty_event(1, b"fresh"));
    assert!(matches!(
        drain(&mut rx).as_slice(),
        [sharer::DownstreamMessage::EventsProcessedAck {
            latest_processed_event_no: 1
        }]
    ));
}

#[test]
fn rejoin_replays_only_missed_events() {
    let f = fixture(HubConfig::default());
    for event_no in 0..3 {
        f.hub
            .handle_sharer_message(f.session_id, f.sharer_conn, pty_event(event_no, b"x"));
    }
    let (viewer_id, conn, _rx) = join(&f, viewer_init(None, None));
    f.hub.viewer_disconnected(f.session_id, &viewer_id, conn);

    let (_, _, mut rx) = join(&f, viewer_init(Some(viewer_id), Some(1)));
    let messages = drain(&mut rx);
    assert!(matches!(
        messages[0],
        viewer::DownstreamMessage::RejoinedSuccessfully { .. }
    ));
    let replayed: Vec<usize> = messages
        .iter()
        .filter_map(|m| match m {
            viewer::DownstreamMessage::OrderedTerminalEvent(e) => Some(e.event_no),
            _ => None,
        })
        .collect();
    assert_eq!(replayed, vec![2]);
}

#[test]
fn exceeding_the_size_cap_terminates_the_session() {
    let mut f = fixture(HubConfig {
        max_session_bytes: 1024,
        ..Default::default()
    });
    let (_, _, mut rx) = join(&f, viewer_init(None, None));
    drain(&mut rx);
    drain(&mut f.sharer_rx);

    f.hub
        .handle_sharer_message(f.session_id, f.sharer_conn, pty_event(0, &[b'x'; 2048]));
    assert!(matches!(
        drain(&mut f.sharer_rx).as_slice(),
        [sharer::DownstreamMessage::SessionTerminated {
            reason: SessionTerminatedReason::ExceededSizeLimit
        }]
    ));
    assert!(matches!(
        drain(&mut rx).as_slice(),
        [viewer::DownstreamMessage::SessionEnded {
            reason: viewer::SessionEndedReason::ExceededSizeLimit
        }]
    ));
    assert!(f.hub.list_sessions().is_empty());
}

#[test]
fn oversized_scrollback_is_refused() {
    let hub = Hub::new(HubConfig {
        max_session_bytes: 64,
        ..Default::default()
    });
    let (tx, mut rx) = mpsc::unbounded_channel();
    assert!(
        hub.create_session(sharer_init(1024), SharerLabels::default(), tx)
            .is_none()
    );
    assert!(matches!(
        drain(&mut rx).as_slice(),
        [sharer::DownstreamMessage::FailedToInitializeSession {
            reason: FailedToInitializeSessionReason::ScrollbackTooLarge {}
        }]
    ));
}

#[test]
fn end_session_notifies_viewers_and_forgets_the_session() {
    let f = fixture(HubConfig::default());
    let (_, _, mut rx) = join(&f, viewer_init(None, None));
    drain(&mut rx);

    f.hub.handle_sharer_message(
        f.session_id,
        f.sharer_conn,
        sharer::UpstreamMessage::EndSession {
            reason: sharer::SessionEndedReason::EndedBySharer,
        },
    );
    assert!(matches!(
        drain(&mut rx).as_slice(),
        [viewer::DownstreamMessage::SessionEnded {
            reason: viewer::SessionEndedReason::EndedBySharer
        }]
    ));
    assert!(f.hub.list_sessions().is_empty());
}

#[test]
fn abandoned_sessions_are_reaped_after_the_grace_period() {
    let f = fixture(HubConfig {
        sharer_reconnect_grace: Duration::from_secs(60),
        ..Default::default()
    });
    f.hub.sharer_disconnected(f.session_id, f.sharer_conn);

    f.hub.reap_abandoned(Instant::now());
    assert_eq!(f.hub.list_sessions().len(), 1);

    f.hub
        .reap_abandoned(Instant::now() + Duration::from_secs(61));
    assert!(f.hub.list_sessions().is_empty());
}

#[test]
fn dashboard_summary_reflects_session_state() {
    let f = fixture(HubConfig::default());
    f.hub.handle_sharer_message(
        f.session_id,
        f.sharer_conn,
        sharer::UpstreamMessage::OrderedTerminalEvent(OrderedTerminalEvent {
            event_no: 0,
            event_type: OrderedTerminalEventType::CommandExecutionStarted {
                participant_id: ParticipantId::new(),
                ai_metadata: None,
            },
        }),
    );
    let (_viewer_id, _conn, _rx) = join(&f, viewer_init(None, None));

    let sessions = f.hub.list_sessions();
    assert_eq!(sessions.len(), 1);
    let summary = &sessions[0];
    assert_eq!(summary.id, f.session_id.to_string());
    assert_eq!(summary.host.as_deref(), Some("laptop"));
    assert_eq!(summary.pwd.as_deref(), Some("/home/me/project"));
    assert!(summary.sharer_connected);
    assert!(summary.command_running);
    assert_eq!(summary.viewer_count, 1);
    assert_eq!((summary.num_rows, summary.num_cols), (40, 120));
}
