use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{LogTail, MAX_LOG_LINES, dashboard_url, load_or_create_token};

/// A path in a new directory that doesn't exist yet.
fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!("synth-rc-{}", uuid::Uuid::new_v4()))
        .join(name)
}

#[test]
fn unspecified_bind_addresses_map_to_loopback() {
    assert_eq!(
        dashboard_url("0.0.0.0:8787".parse().unwrap()),
        "http://127.0.0.1:8787"
    );
    assert_eq!(
        dashboard_url("[::]:8787".parse().unwrap()),
        "http://[::1]:8787"
    );
}

#[test]
fn specific_bind_addresses_are_kept() {
    assert_eq!(
        dashboard_url("192.0.2.7:9000".parse().unwrap()),
        "http://192.0.2.7:9000"
    );
}

#[test]
fn a_generated_token_is_saved_and_reused() {
    let path = temp_path("token");
    let first = load_or_create_token(&path).unwrap();
    assert!(first.len() >= 16);
    assert_eq!(load_or_create_token(&path).unwrap(), first);
}

#[test]
fn a_saved_token_is_trimmed_to_its_first_line() {
    let path = temp_path("token");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "  saved-token \nignored\n").unwrap();
    assert_eq!(load_or_create_token(&path).unwrap(), "saved-token");
}

#[test]
fn an_empty_token_file_gets_a_new_token() {
    let path = temp_path("token");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "\n").unwrap();
    let token = load_or_create_token(&path).unwrap();
    assert!(!token.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), token);
}

#[test]
fn the_log_reaches_the_file_and_the_window_text() {
    let path = temp_path("server.log");
    let mut log = LogTail::create(&path, || {}).unwrap();
    log.write_all(b"first\nsecond\r\n").unwrap();
    assert_eq!(log.text(), "first\r\nsecond");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\r\n");
}

#[test]
fn a_line_split_across_writes_is_joined() {
    let mut log = LogTail::create(&temp_path("server.log"), || {}).unwrap();
    log.write_all(b"par").unwrap();
    assert_eq!(log.text(), "");
    log.write_all(b"tial\n").unwrap();
    assert_eq!(log.text(), "partial");
}

#[test]
fn only_the_latest_lines_are_kept_for_the_window() {
    let mut log = LogTail::create(&temp_path("server.log"), || {}).unwrap();
    for i in 0..=MAX_LOG_LINES {
        writeln!(log, "line {i}").unwrap();
    }
    let text = log.text();
    assert_eq!(text.lines().count(), MAX_LOG_LINES);
    assert!(text.starts_with("line 1\r\n"));
}

#[test]
fn each_write_notifies() {
    let writes = Arc::new(AtomicUsize::new(0));
    let counter = writes.clone();
    let mut log = LogTail::create(&temp_path("server.log"), move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    log.write_all(b"one\n").unwrap();
    log.write_all(b"two\n").unwrap();
    assert_eq!(writes.load(Ordering::SeqCst), 2);
}
