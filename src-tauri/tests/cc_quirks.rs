//! The CC quirks profile (D7) against what was recorded from real CC: the
//! screens in `fixtures/cc_screens/` and `claude --version` output. The
//! golden file pins every value of the current profile, so a change to a
//! glyph, text, key, delay or path shows in review. Rewrite it with
//! `C9_UPDATE_GOLDEN=1`.

#![cfg(all(unix, feature = "engine"))]

use c9watch_lib::engine::adapter::cc::quirks::{self, CURRENT};
use std::path::Path;

const GOLDEN: &str = "tests/fixtures/cc_quirks/cc-2.1.golden.txt";
const VERSION_2_1_316: &str = include_str!("fixtures/cc_quirks/cc2.1.316_version.txt");

#[test]
fn the_current_profile_matches_its_golden() {
    let got = format!("{:#?}\n", CURRENT);
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if std::env::var("C9_UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(&path, &got).unwrap();
    }
    let want = std::fs::read_to_string(&path).unwrap();
    assert_eq!(got, want, "the CC quirks profile changed; see {}", GOLDEN);
}

#[test]
fn recorded_version_output_passes_the_gate() {
    let p = quirks::gate(Ok(VERSION_2_1_316.into()), false).unwrap();
    assert_eq!(p.name, CURRENT.name);
}

#[test]
fn every_recorded_screen_is_from_a_covered_version() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cc_screens");
    let mut n = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        let v = name
            .strip_prefix("cc")
            .and_then(|s| s.split('_').next())
            .and_then(quirks::parse_version)
            .unwrap_or_else(|| panic!("{} does not name its CC version", name));
        assert!(CURRENT.covers(v), "{} is outside {}", name, CURRENT.range());
        n += 1;
    }
    assert!(n >= 10, "only {} recorded screens", n);
}
