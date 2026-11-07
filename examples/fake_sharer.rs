//! Scripted stand-in for a Warp terminal, used by the Playwright tests in `web-tests/`.
//!
//! Shares a session with a running server using the real protocol types and PTY framing, then
//! follows JSON commands on stdin and reports what it receives as JSON lines on stdout.
//!
//! ```text
//! fake_sharer <host:port> <token> [--running] [--alt-screen] [--pwd <dir>] [--output <text>]
//!
//! stdin:  {"op":"output","text":"..."} | {"op":"started"} | {"op":"finished"}
//! stdout: {"session_id":"..."} | {"pty":[...bytes]} | {"command":"..."}
//! ```

use futures_util::{SinkExt, StreamExt};
use session_sharing_protocol::common::{
    ActivePrompt, BlockId, FeatureSupport, InputReplicaId, OrderedTerminalEvent,
    OrderedTerminalEventType, ParticipantId, Scrollback, ScrollbackBlock, Selection, UserID,
    WindowSize,
};
use session_sharing_protocol::sharer;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[derive(Default)]
struct Options {
    running: bool,
    alt_screen: bool,
    pwd: Option<String>,
    output: Option<String>,
}

fn parse_options(args: impl Iterator<Item = String>) -> Options {
    let mut options = Options::default();
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--running" => options.running = true,
            "--alt-screen" => options.alt_screen = true,
            "--pwd" => options.pwd = args.next(),
            "--output" => options.output = args.next(),
            other => panic!("unknown argument {other}"),
        }
    }
    options
}

/// The newest block, shaped like the client's `SerializedBlock`: an unfinished block is a running
/// command when `did_execute` is set and an idle prompt otherwise.
fn active_block(options: &Options) -> ScrollbackBlock {
    let block = serde_json::json!({
        "pwd": options.pwd,
        "stylized_command": if options.running { b"sleep 100".to_vec() } else { Vec::new() },
        "stylized_output": options.output.as_deref().unwrap_or_default().as_bytes(),
        "did_execute": options.running,
        "start_ts": options.running.then_some("2026-11-05T21:00:00-06:00"),
        "completed_ts": null,
    });
    ScrollbackBlock {
        raw: serde_json::to_vec(&block).expect("block serializes"),
    }
}

fn init_payload(options: &Options) -> sharer::InitPayload {
    sharer::InitPayload {
        scrollback: Scrollback {
            blocks: vec![active_block(options)],
            is_alt_screen_active: options.alt_screen,
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

fn event_for(command: &serde_json::Value) -> OrderedTerminalEventType {
    match command["op"].as_str() {
        Some("output") => OrderedTerminalEventType::PtyBytesRead {
            bytes: lz4_flex::block::compress_prepend_size(
                command["text"].as_str().unwrap_or_default().as_bytes(),
            ),
        },
        Some("started") => OrderedTerminalEventType::CommandExecutionStarted {
            participant_id: ParticipantId::default(),
            ai_metadata: None,
        },
        Some("finished") => OrderedTerminalEventType::CommandExecutionFinished {
            next_block_id: BlockId::from("b1".to_owned()),
        },
        other => panic!("unknown op {other:?}"),
    }
}

/// Reports what the server forwarded to the "PTY". Other messages don't affect these tests.
fn report(message: sharer::DownstreamMessage) {
    match message {
        sharer::DownstreamMessage::SessionInitialized { session_id, .. } => {
            println!(
                "{}",
                serde_json::json!({ "session_id": session_id.to_string() })
            );
        }
        sharer::DownstreamMessage::WriteToPtyRequested { bytes, .. } => {
            println!("{}", serde_json::json!({ "pty": bytes }));
        }
        sharer::DownstreamMessage::CommandExecutionRequested { command, .. } => {
            println!("{}", serde_json::json!({ "command": command }));
        }
        _ => {}
    }
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().expect("host:port");
    let token = args.next().expect("token");
    let options = parse_options(args);

    let mut request = format!("ws://{host}/sessions/create?version=v0.test")
        .into_client_request()
        .expect("valid url");
    let headers = request.headers_mut();
    headers.insert(
        "authorization",
        format!("Bearer {token}").parse().expect("valid token"),
    );
    headers.insert("x-synth-host", "fake-host".parse().expect("valid host"));
    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("sharer connects");

    let init = sharer::UpstreamMessage::Initialize(init_payload(&options))
        .to_json()
        .expect("init serializes");
    ws.send(Message::Text(init.into()))
        .await
        .expect("init sent");

    // Stdin blocks, so read it on a thread. Its end means the test is done with this terminal.
    let (commands_tx, mut commands) = mpsc::unbounded_channel::<serde_json::Value>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            let command = serde_json::from_str(&line).expect("stdin line is JSON");
            if commands_tx.send(command).is_err() {
                return;
            }
        }
    });

    let mut event_no = 0;
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { return };
                let event = sharer::UpstreamMessage::OrderedTerminalEvent(OrderedTerminalEvent {
                    event_no,
                    event_type: event_for(&command),
                });
                event_no += 1;
                let json = event.to_json().expect("event serializes");
                ws.send(Message::Text(json.into())).await.expect("event sent");
            }
            frame = ws.next() => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        report(sharer::DownstreamMessage::from_json(&text).expect("valid message"));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return,
                }
            }
        }
    }
}
