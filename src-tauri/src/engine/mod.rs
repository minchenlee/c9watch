//! Agent engine. Compiled only with the `engine` feature, and only on unix.

#[cfg(unix)]
pub mod home;

#[cfg(unix)]
pub mod fsutil;

#[cfg(unix)]
pub mod lock;

#[cfg(unix)]
pub mod sock;

#[cfg(unix)]
pub mod peer;

#[cfg(unix)]
pub mod proto;

#[cfg(unix)]
pub mod api;

#[cfg(unix)]
pub mod principal;

#[cfg(unix)]
pub mod authz;

#[cfg(unix)]
pub mod confirmation;

#[cfg(unix)]
pub mod agents;

#[cfg(unix)]
pub mod agent_bin;

#[cfg(unix)]
pub mod db;

#[cfg(unix)]
pub mod machine;

#[cfg(unix)]
pub mod detect;

#[cfg(unix)]
pub mod views;

#[cfg(unix)]
pub mod discovery;

#[cfg(all(unix, feature = "engine"))]
pub mod sensitivity;

#[cfg(unix)]
pub mod import;

#[cfg(unix)]
pub mod driver;

#[cfg(unix)]
pub mod pty;

#[cfg(unix)]
pub mod vt;

#[cfg(unix)]
pub mod cc_screen;

#[cfg(unix)]
pub mod adapter;

#[cfg(unix)]
pub mod transcript;

#[cfg(unix)]
pub mod merge;

#[cfg(unix)]
pub mod generation;

#[cfg(unix)]
pub mod hooks;

#[cfg(unix)]
pub mod standing;

#[cfg(unix)]
pub mod ops;

#[cfg(unix)]
pub mod create;

#[cfg(unix)]
pub mod mail;

#[cfg(unix)]
pub mod drain;

#[cfg(unix)]
pub mod run;

#[cfg(all(unix, feature = "live-test"))]
pub mod live_test;

#[cfg(unix)]
pub mod cc_inbox;
#[cfg(unix)]
pub mod cc_listener;
#[cfg(unix)]
pub mod peer_log;
