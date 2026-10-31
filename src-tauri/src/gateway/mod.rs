//! Shared gateway security. GUI and headless core compile the same logic.
pub mod assets;
pub mod auth;
pub mod limits;
pub mod policy;
pub mod security;

pub mod redaction;

#[cfg(feature = "engine")]
pub mod tokens;

pub mod consent;
