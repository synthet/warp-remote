//! Windows tray app for the remote-control server: no console, and a small status window that
//! collapses to the notification area.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;

#[cfg(windows)]
fn main() {
    app::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The tray app is Windows-only; use synth-remote-control-server.");
    std::process::exit(1);
}
