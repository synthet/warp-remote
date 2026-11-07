//! Platform-independent parts of the tray app: the dashboard address, the saved token, and the log
//! kept for the window.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;

use crate::auth;

/// Lines of log kept for the window. The log file keeps everything from this run.
const MAX_LOG_LINES: usize = 500;

/// The address to open on this machine. An unspecified bind address can't be browsed to, so it maps
/// to loopback.
pub fn dashboard_url(bind: SocketAddr) -> String {
    let ip = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("http://{}", SocketAddr::new(ip, bind.port()))
}

/// Reads the token saved at `path`, or generates and saves one. A saved token keeps working across
/// restarts, which a tray app needs because nobody sees a token printed at startup.
pub fn load_or_create_token(path: &Path) -> anyhow::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let token = contents.lines().next().unwrap_or_default().trim();
            if !token.is_empty() {
                return Ok(token.to_owned());
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("reading token file {}", path.display()));
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let token = auth::generate_token();
    std::fs::write(path, &token).with_context(|| format!("writing {}", path.display()))?;
    Ok(token)
}

struct LogState {
    file: File,
    lines: VecDeque<String>,
    partial: String,
}

/// A log sink that writes to a file and keeps the latest lines for display. Clones share one log.
#[derive(Clone)]
pub struct LogTail {
    state: Arc<Mutex<LogState>>,
    on_write: Arc<dyn Fn() + Send + Sync>,
}

impl LogTail {
    /// Starts a new log at `path`, replacing the previous run's. `on_write` runs after each write,
    /// on the writing thread.
    pub fn create(
        path: &Path,
        on_write: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
        Ok(Self {
            state: Arc::new(Mutex::new(LogState {
                file,
                lines: VecDeque::new(),
                partial: String::new(),
            })),
            on_write: Arc::new(on_write),
        })
    }

    /// The kept lines, with Windows line endings for a text box.
    pub fn text(&self) -> String {
        let state = self.state.lock().expect("log lock");
        Vec::from_iter(state.lines.iter().map(String::as_str)).join("\r\n")
    }
}

impl Write for LogTail {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        {
            let mut state = self.state.lock().expect("log lock");
            state.file.write_all(buf)?;
            state.partial.push_str(&String::from_utf8_lossy(buf));
            while let Some(end) = state.partial.find('\n') {
                let line = state.partial[..end].trim_end_matches('\r').to_owned();
                state.partial.drain(..=end);
                state.lines.push_back(line);
                if state.lines.len() > MAX_LOG_LINES {
                    state.lines.pop_front();
                }
            }
        }
        (self.on_write)();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.state.lock().expect("log lock").file.flush()
    }
}

#[cfg(test)]
#[path = "tray_support_tests.rs"]
mod tests;
