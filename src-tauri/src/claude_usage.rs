//! Local Claude Code status-line bridge. Stores quota fields only, never the full payload.
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const MAX_INPUT: u64 = 1_048_576;
pub fn config_dir() -> Result<PathBuf, String> {
    Ok(std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or(
            dirs::home_dir()
                .ok_or("Home directory unavailable")?
                .join(".claude"),
        ))
}
pub fn cache_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("c9watch/subscription-usage.json"))
}
pub fn sanitize(value: &Value, now: i64) -> Value {
    let mut limits = serde_json::Map::new();
    for key in ["five_hour", "seven_day", "spend_limit"] {
        let w = &value["rate_limits"][key];
        if let (Some(percent), Some(reset)) =
            (w["used_percentage"].as_f64(), w["resets_at"].as_i64())
        {
            if percent.is_finite() && percent >= 0.0 && reset > now {
                limits.insert(
                    key.into(),
                    json!({"used_percentage":percent,"resets_at":reset}),
                );
            }
        }
    }
    json!({"schemaVersion":1,"updatedAt":now,"rate_limits":limits})
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid snapshot path")?;
    fs::create_dir_all(parent).map_err(|_| "Cannot create usage directory")?;
    let temp = parent.join(format!(".usage-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp)
            .map_err(|_| "Cannot create usage snapshot")?;
        file.write_all(bytes)
            .map_err(|_| "Cannot write usage snapshot")?;
        file.sync_all().map_err(|_| "Cannot flush usage snapshot")?;
        fs::rename(&temp, path).map_err(|_| "Cannot replace usage snapshot")
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(str::to_owned)
}
pub fn record(input: &[u8], path: &Path, now: i64) -> Result<Value, String> {
    let value: Value = serde_json::from_slice(input).map_err(|_| "Invalid status-line JSON")?;
    if !value.is_object() {
        return Err("Expected status-line object".into());
    }
    let snapshot = sanitize(&value, now);
    atomic_write(
        path,
        &serde_json::to_vec(&snapshot).map_err(|_| "Cannot encode usage")?,
    )?;
    Ok(snapshot)
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub fn configured(mut settings: Value, executable: &Path) -> Result<Value, String> {
    if !settings.is_object() {
        return Err("Claude settings must be a JSON object".into());
    }
    let command = format!(
        "{} usage-bridge",
        shell_quote(executable.to_str().ok_or("Invalid executable path")?)
    );
    if settings["statusLine"].is_null() {
        settings["statusLine"] = json!({"type":"command","command":command});
    } else {
        if settings["statusLine"]["type"] != "command" {
            return Err("Unsupported status line type; configure bridge manually".into());
        }
        let existing = settings["statusLine"]["command"]
            .as_str()
            .ok_or("Invalid status line command")?;
        if existing == command || existing.starts_with(&format!("{command} --passthrough | (\n")) {
            return Ok(settings);
        }
        settings["statusLine"]["command"] =
            json!(format!("{command} --passthrough | (\n{existing}\n)"));
    }
    Ok(settings)
}
pub fn install(directory: &Path, executable: &Path) -> Result<String, String> {
    #[cfg(windows)]
    {
        let _ = (directory, executable);
        return Err("Configure usage-bridge manually in your status line shell on Windows".into());
    }
    #[cfg(not(windows))]
    {
        let path = directory.join("settings.json");
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("Claude settings is a symlink; configure bridge manually".into());
        }
        let original = match fs::read(&path) {
            Ok(v) => Some(v),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err("Cannot read Claude settings".into()),
        };
        let settings = match original.as_ref() {
            Some(bytes) => {
                serde_json::from_slice(bytes).map_err(|_| "Invalid Claude settings JSON")?
            }
            None => json!({}),
        };
        let next = configured(settings.clone(), executable)?;
        if next == settings {
            return Ok("Claude usage bridge is already enabled".into());
        }
        let backup = directory.join(format!(
            "settings.c9watch-backup-{}.json",
            uuid::Uuid::new_v4()
        ));
        let had_settings = original.is_some();
        if let Some(bytes) = original {
            atomic_write(&backup, &bytes)?;
        }
        atomic_write(
            &path,
            &serde_json::to_vec_pretty(&next).map_err(|_| "Cannot encode settings")?,
        )?;
        Ok(if had_settings {
            format!(
                "Claude usage bridge enabled. Previous settings backup: {}",
                backup.display()
            )
        } else {
            "Claude usage bridge enabled. No previous settings file existed.".into()
        })
    }
}
pub fn run(install_bridge: bool, passthrough: bool) -> Result<(), String> {
    if install_bridge {
        println!(
            "{}",
            install(
                &config_dir()?,
                &std::env::current_exe().map_err(|_| "Cannot locate c9watch executable")?
            )?
        );
        return Ok(());
    }
    let mut input = Vec::new();
    std::io::stdin()
        .take(MAX_INPUT + 1)
        .read_to_end(&mut input)
        .map_err(|_| "Cannot read status line")?;
    if input.len() as u64 > MAX_INPUT {
        return Err("Status-line input exceeds 1 MiB".into());
    }
    let result =
        cache_path().and_then(|path| record(&input, &path, chrono::Utc::now().timestamp()));
    // Keep an existing status line functional even when snapshot storage fails.
    if passthrough {
        std::io::stdout()
            .write_all(&input)
            .map_err(|_| "Cannot forward status line")?;
    }
    match result {
        Ok(snapshot) if !passthrough => {
            let values: Vec<String> = [
                ("five_hour", "5h"),
                ("seven_day", "7d"),
                ("spend_limit", "spend"),
            ]
            .into_iter()
            .filter_map(|(key, label)| {
                snapshot["rate_limits"][key]["used_percentage"]
                    .as_f64()
                    .map(|p| format!("{label} {p:.0}%"))
            })
            .collect();
            println!(
                "Claude Code · {}",
                if values.is_empty() {
                    "usage unavailable".into()
                } else {
                    values.join(" · ")
                }
            );
        }
        Err(error) if passthrough => eprintln!("c9watch: {error}"),
        Err(error) => return Err(error),
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_quota_fields_are_saved_and_empty_input_clears_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let input = json!({"session_id":"private","cwd":"private","rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":200},"seven_day":{"used_percentage":0,"resets_at":300},"spend_limit":{"used_percentage":120,"resets_at":300},"unknown":{"secret":"private"}}});
        let saved = record(&serde_json::to_vec(&input).unwrap(), &path, 100).unwrap();
        assert_eq!(saved["rate_limits"].as_object().unwrap().len(), 3);
        assert!(!fs::read_to_string(&path).unwrap().contains("private"));
        record(b"{}", &path, 101).unwrap();
        assert!(
            serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap()["rate_limits"]
                .as_object()
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn invalid_and_reset_limits_are_absent_not_zero() {
        let v = sanitize(
            &json!({"rate_limits":{"five_hour":{"used_percentage":-1,"resets_at":200},"seven_day":{"used_percentage":40,"resets_at":100},"spend_limit":{"used_percentage":"50","resets_at":200}}}),
            100,
        );
        assert!(v["rate_limits"].as_object().unwrap().is_empty());
    }
    #[test]
    fn install_preserves_settings_backs_up_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = json!({"permissions":{"deny":["something"]},"statusLine":{"type":"command","command":"cat # existing comment","padding":2}});
        let original = serde_json::to_vec(&settings).unwrap();
        fs::write(&path, &original).unwrap();
        let exe = Path::new("/some path/c9'watch");
        install(dir.path(), exe).unwrap();
        let next: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(next["permissions"], settings["permissions"]);
        assert_eq!(next["statusLine"]["padding"], 2);
        assert!(next["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("cat # existing comment\n)"));
        assert_eq!(configured(next.clone(), exe).unwrap(), next);
        let backup = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("settings.c9watch-backup-")
            })
            .unwrap();
        assert_eq!(fs::read(backup).unwrap(), original);
    }
}
