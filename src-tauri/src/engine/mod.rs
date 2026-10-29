//! Agent engine. Compiled only with the `engine` feature, and only on unix.

#[cfg(unix)]
pub mod home;
