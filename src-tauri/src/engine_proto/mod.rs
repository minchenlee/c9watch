//! What a c9p/1 client needs from the engine, outside the `engine` feature:
//! the wire types, the channel and engine home, the peer uid check, and a
//! small blocking client. A normal GUI build has no `engine` feature, so the
//! dev GUI's shadow diff reads the engine through this module. The engine
//! re-exports these items, so there is one copy of each.

pub mod client;
pub mod home;
pub mod peer;
pub mod session_view;
pub mod wire;

/// The shadow diff exists only in dev channel builds (C1a-17).
#[cfg(c9_channel_dev)]
pub mod shadow;

#[cfg(test)]
mod channel_tests {
    /// Whether this test binary holds `needle`. The needle is passed in
    /// reverse, so the test's own literal never matches.
    fn exe_contains_reversed(reversed: &str) -> bool {
        let needle: Vec<u8> = reversed.bytes().rev().collect();
        let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        exe.windows(needle.len()).any(|w| w == needle.as_slice())
    }

    /// The prod check below would pass on any binary if the scan were
    /// broken, so check that it finds the shadow diff in a dev build.
    #[cfg(c9_channel_dev)]
    #[test]
    fn dev_channel_build_has_shadow_diff() {
        // No literal of the name here: the binary must hold it from the
        // shadow module itself.
        assert!(!super::shadow::DIFF_FILE.is_empty());
        assert!(exe_contains_reversed("nosjdn.ffid-wodahs"));
    }

    /// A `C9WATCH_CHANNEL=prod` build has no shadow diff: no module, and
    /// none of its file names in the binary. CI runs this in the prod
    /// channel job. (`cli::dispatch_tests::shadow_report_only_in_dev_builds`
    /// checks the CLI command.)
    #[cfg(not(c9_channel_dev))]
    #[test]
    fn prod_channel_build_has_no_shadow_diff() {
        assert_eq!(super::home::Channel::current(), super::home::Channel::Prod);
        assert!(!exe_contains_reversed("nosjdn.ffid-wodahs"));
        assert!(!exe_contains_reversed("nosjdn.egarevoc-wodahs"));
    }
}

pub mod compatibility;
pub mod fsutil;
pub mod lock;
pub mod spawn;

pub mod confirmation;

pub mod annotations;
