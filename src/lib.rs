//! Self-hosted remote control for Synth Warp.
//!
//! A drop-in replacement for Warp's hosted session-sharing service: it speaks the public
//! `session-sharing-protocol`, so the existing sharer code in the client needs only a server URL
//! and a token. It also serves a web/mobile app that lists every shared terminal and drives it.

pub mod auth;
pub mod hub;
pub mod server;
mod web;

pub use hub::{Hub, HubConfig};
pub use server::{AppState, router, spawn_reaper};
