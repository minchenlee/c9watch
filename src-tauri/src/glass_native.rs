//! Native glass facts for the glass shell (glass build plan G10b,
//! ui-refactor-direction §2.1). The main window has `transparent: true` and
//! the macOS `sidebar` vibrancy (`tauri.conf.json`). The glass shell asks
//! this command once at start and again on each window focus:
//!
//!   native_glass → { vibrancy, reduceTransparency }
//!
//! - `vibrancy`: the window shows a native material behind the WebView, so
//!   glass may make `html, body` transparent. True on macOS only. Classic
//!   never calls this and keeps its opaque body.
//! - `reduceTransparency`: System Settings › Accessibility › Display ›
//!   Reduce transparency. WebKit does not support
//!   `prefers-reduced-transparency`, so Rust reads
//!   `NSWorkspace.accessibilityDisplayShouldReduceTransparency`. When it is
//!   true, glass sets `data-glass="off"` whatever the user's pref is.
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeGlass {
    pub vibrancy: bool,
    pub reduce_transparency: bool,
}

/// The current values. Cheap: one property read, no cache, so a change in
/// System Settings shows on the next call.
pub fn read() -> NativeGlass {
    NativeGlass {
        vibrancy: cfg!(target_os = "macos"),
        reduce_transparency: reduce_transparency(),
    }
}

#[cfg(target_os = "macos")]
fn reduce_transparency() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency()
}

#[cfg(not(target_os = "macos"))]
fn reduce_transparency() -> bool {
    false
}

/// `localCommand('native_glass')`. Main window only (capabilities).
#[tauri::command]
pub fn native_glass() -> NativeGlass {
    read()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_camel_case_for_the_glass_shell() {
        let v = serde_json::to_value(NativeGlass {
            vibrancy: true,
            reduce_transparency: false,
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "vibrancy": true, "reduceTransparency": false })
        );
    }

    /// Tauri's own merge (RFC 7396) of `tauri.conf.json` and
    /// `tauri.<platform>.conf.json`, the config each build really uses.
    fn merged_main(target: tauri::utils::platform::Target) -> serde_json::Value {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let (config, _) = tauri::utils::config::parse::read_from(target, dir).unwrap();
        let windows = config["app"]["windows"].as_array().unwrap().clone();
        assert_eq!(windows.len(), 2, "main and popover");
        assert_eq!(windows[1]["label"], "popover");
        assert_eq!(windows[1]["transparent"], true, "popover unchanged");
        windows[0].clone()
    }

    #[test]
    fn main_window_glass_is_macos_only_in_the_merged_config() {
        use tauri::utils::platform::Target;
        let mac = merged_main(Target::MacOS);
        assert_eq!(mac["label"], "main");
        assert_eq!(mac["transparent"], true);
        assert_eq!(
            mac["windowEffects"]["effects"],
            serde_json::json!(["sidebar"])
        );
        assert_eq!(mac["theme"], "Dark");
        assert_eq!(mac["titleBarStyle"], "Overlay");
        for target in [Target::Linux, Target::Windows] {
            let main = merged_main(target);
            for key in ["transparent", "windowEffects", "theme"] {
                assert!(main.get(key).is_none(), "{target:?} main has {key}");
            }
        }
    }

    #[test]
    fn vibrancy_is_macos_only_and_the_system_read_does_not_panic() {
        let v = read();
        assert_eq!(v.vibrancy, cfg!(target_os = "macos"));
        if !cfg!(target_os = "macos") {
            assert!(!v.reduce_transparency);
        }
    }
}
