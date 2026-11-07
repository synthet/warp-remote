//! Settings and startup shared by the console server and the Windows tray app.

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::Parser;
use session_sharing_protocol::common::Role;
use tokio::net::TcpListener;

use crate::{AppState, Hub, HubConfig, router, spawn_reaper};

/// Self-hosted remote-control server for Synth Warp terminals.
#[derive(Debug, Parser)]
#[command(name = "synth-remote-control-server", version)]
pub struct Args {
    /// Address to listen on; the default covers every local IPv4 address. Put a TLS-terminating
    /// proxy in front for anything but a trusted LAN or tailnet.
    #[arg(long, env = "SYNTH_RC_BIND", default_value = "0.0.0.0:8787")]
    pub bind: SocketAddr,

    /// Shared secret that authorizes terminals and the web app. Generated and printed if unset.
    #[arg(long, env = "SYNTH_RC_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// Read the shared secret from a file instead (first line, trimmed).
    #[arg(long, env = "SYNTH_RC_TOKEN_FILE", conflicts_with = "token")]
    token_file: Option<PathBuf>,

    /// Name shown for the terminal owner in participant lists.
    #[arg(long, env = "SYNTH_RC_OWNER_NAME", default_value = "Owner")]
    owner_name: String,

    /// Role granted to viewers on join: `reader` (watch only), `executor`, or `full`.
    #[arg(long, env = "SYNTH_RC_VIEWER_ROLE", default_value = "full")]
    viewer_role: String,

    /// Per-session memory cap for scrollback plus live output, in MiB.
    #[arg(long, env = "SYNTH_RC_MAX_SESSION_MIB", default_value_t = 256)]
    max_session_mib: u64,

    /// Minutes a session is kept while its terminal is disconnected.
    #[arg(long, env = "SYNTH_RC_RECONNECT_GRACE_MINUTES", default_value_t = 10)]
    reconnect_grace_minutes: u64,
}

fn parse_role(role: &str) -> anyhow::Result<Role> {
    match role.to_ascii_lowercase().as_str() {
        "reader" => Ok(Role::Reader),
        "executor" => Ok(Role::Executor),
        "full" => Ok(Role::Full),
        other => anyhow::bail!("unknown viewer role {other:?}; use reader, executor, or full"),
    }
}

impl Args {
    /// The token from `--token-file` or `--token`, if either is set.
    pub fn configured_token(&self) -> anyhow::Result<Option<String>> {
        if let Some(path) = &self.token_file {
            let contents = std::fs::read_to_string(path)
                .with_context(|| format!("reading token file {}", path.display()))?;
            let token = contents
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned();
            anyhow::ensure!(!token.is_empty(), "token file {} is empty", path.display());
            return Ok(Some(token));
        }
        Ok(self
            .token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned))
    }
}

/// Binds the listen address, so callers can report a failure before serving.
pub async fn bind(args: &Args) -> anyhow::Result<TcpListener> {
    let listener = TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("binding {}", args.bind))?;
    log::info!(
        "Synth Warp remote control listening on http://{}",
        args.bind
    );
    Ok(listener)
}

/// Serves on `listener` until `shutdown` completes.
pub async fn serve(
    listener: TcpListener,
    args: &Args,
    token: String,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    if token.len() < 16 {
        log::warn!("The remote-control token is shorter than 16 characters; use a longer secret");
    }

    let max_session_bytes = args.max_session_mib.saturating_mul(1024 * 1024);
    let hub = Arc::new(Hub::new(HubConfig {
        owner_uid: "synth-owner".to_owned(),
        owner_display_name: args.owner_name.clone(),
        max_session_bytes,
        sharer_reconnect_grace: Duration::from_secs(args.reconnect_grace_minutes * 60),
        viewer_role: parse_role(&args.viewer_role)?,
    }));
    spawn_reaper(hub.clone());

    let state = AppState {
        hub,
        token: token.into(),
        max_message_bytes: usize::try_from(max_session_bytes).unwrap_or(usize::MAX),
    };
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}
