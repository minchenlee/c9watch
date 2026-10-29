//! The machine key (C1a-12): an ed25519 key pair that names this machine.
//!
//! The private key lives in `<home>/machine.key`, outside the db, so a db
//! that is recreated (new epoch) keeps the same machine id. The machine id
//! is the public key in lowercase hex. It goes in `machines.id`, in the
//! `hello` result and in every cursor (`machine:epoch:seq`, #43).
//!
//! The file holds the 32-byte secret seed and nothing else, mode 0600.
//! Creation never shows a half-written file: the seed goes to a unique temp
//! file opened with `O_CREAT|O_EXCL`, is synced, and is then published with
//! `link(2)`, which fails when `machine.key` already exists. When two
//! processes race, one link wins and the other process reads the winner's
//! key.

use ed25519_dalek::SigningKey;
use rand::RngCore;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// The key file inside the engine home.
pub const KEY_NAME: &str = "machine.key";

/// Length of the secret seed in the key file.
const SEED_LEN: usize = 32;

/// `<home>/machine.key`
pub fn key_path(home: &Path) -> PathBuf {
    home.join(KEY_NAME)
}

/// This machine's key. No `Debug`: it holds the private key.
pub struct MachineKey {
    signing: SigningKey,
    id: String,
}

impl MachineKey {
    fn from_seed(seed: &[u8; SEED_LEN]) -> Self {
        let signing = SigningKey::from_bytes(seed);
        let id = hex(signing.verifying_key().as_bytes());
        MachineKey { signing, id }
    }

    /// The public key in lowercase hex (64 characters).
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn verifying_key(&self) -> ed25519_dalek::VerifyingKey {
        self.signing.verifying_key()
    }
}

/// Load `<home>/machine.key`, or create it when it does not exist. `home`
/// must already be a private directory.
pub fn load_or_create(home: &Path) -> Result<MachineKey, String> {
    load_or_create_inner(home).map(|(key, _)| key)
}

/// `load_or_create`, plus whether this call published the key.
fn load_or_create_inner(home: &Path) -> Result<(MachineKey, bool), String> {
    let path = key_path(home);
    if let Some(key) = load(&path)? {
        return Ok((key, false));
    }
    let won = create(home, &path)?;
    // Read back whatever won the race, ours or another process's.
    let key = load(&path)?.ok_or_else(|| format!("{:?} vanished after it was created", path))?;
    Ok((key, won))
}

/// `Ok(None)` when the file does not exist.
fn load(path: &Path) -> Result<Option<MachineKey>, String> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(format!("{:?} is a symlink, refusing to use it", path));
        }
        Err(e) => return Err(format!("Failed to open {:?}: {}", path, e)),
    };
    let md = file
        .metadata()
        .map_err(|e| format!("Failed to stat {:?}: {}", path, e))?;
    if !md.is_file() {
        return Err(format!("{:?} is not a regular file", path));
    }
    if md.uid() != unsafe { libc::geteuid() } {
        return Err(format!("{:?} is not owned by us", path));
    }
    // A key others could read is no longer private; never use or fix it.
    if md.mode() & 0o077 != 0 {
        return Err(format!(
            "{:?} has mode {:o}, want 0600; move it away to make a new key",
            path,
            md.mode() & 0o777
        ));
    }
    let mut buf = Vec::with_capacity(SEED_LEN + 1);
    (&file)
        .take(SEED_LEN as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("Failed to read {:?}: {}", path, e))?;
    let seed: [u8; SEED_LEN] = buf
        .as_slice()
        .try_into()
        .map_err(|_| format!("{:?} holds {} bytes, want {}", path, md.len(), SEED_LEN))?;
    Ok(Some(MachineKey::from_seed(&seed)))
}

/// Write a new seed and publish it as `path` unless it already exists.
/// Returns whether ours was published; losing the race is not an error.
fn create(home: &Path, path: &Path) -> Result<bool, String> {
    let mut seed = [0u8; SEED_LEN];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let tmp = home.join(format!(
        ".{}.{}.{}.tmp",
        KEY_NAME,
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let written = write_new(&tmp, &seed);
    let linked = written.and_then(|_| match std::fs::hard_link(&tmp, path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(format!("Failed to publish {:?}: {}", path, e)),
    });
    let _ = std::fs::remove_file(&tmp);
    let won = linked?;
    // Make the new name durable before anything depends on it.
    File::open(home)
        .and_then(|d| d.sync_all())
        .map_err(|e| format!("Failed to sync {:?}: {}", home, e))?;
    Ok(won)
}

/// Create `path` with `O_CREAT|O_EXCL`, mode 0600, and write `bytes`.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| format!("Failed to create {:?}: {}", path, e))?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("Failed to write {:?}: {}", path, e))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn id_is_stable_across_restarts() {
        let t = home();
        let first = load_or_create(t.path()).unwrap();
        assert_eq!(first.id().len(), 64);
        assert!(first.id().bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(first.id(), hex(first.verifying_key().as_bytes()));
        let again = load_or_create(t.path()).unwrap();
        assert_eq!(again.id(), first.id());

        let path = key_path(t.path());
        let md = std::fs::metadata(&path).unwrap();
        assert_eq!(md.permissions().mode() & 0o777, 0o600);
        assert_eq!(md.len(), SEED_LEN as u64);
        // No temp file is left behind.
        let names: Vec<_> = std::fs::read_dir(t.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [KEY_NAME]);
    }

    #[test]
    fn other_homes_get_other_ids() {
        let (a, b) = (home(), home());
        assert_ne!(
            load_or_create(a.path()).unwrap().id(),
            load_or_create(b.path()).unwrap().id()
        );
    }

    #[test]
    fn a_bad_key_file_is_refused_and_kept() {
        let t = home();
        let path = key_path(t.path());

        std::fs::write(&path, b"short").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let err = load_or_create(t.path()).map(drop).unwrap_err();
        assert!(err.contains("want 32"), "{}", err);
        assert_eq!(std::fs::read(&path).unwrap(), b"short");

        std::fs::write(&path, [7u8; SEED_LEN]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = load_or_create(t.path()).map(drop).unwrap_err();
        assert!(err.contains("want 0600"), "{}", err);

        std::fs::remove_file(&path).unwrap();
        let target = t.path().join("target");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let err = load_or_create(t.path()).map(drop).unwrap_err();
        assert!(err.contains("symlink"), "{}", err);
        assert!(!target.exists());
    }

    #[test]
    fn threads_racing_for_a_new_key_agree() {
        for _ in 0..20 {
            let t = home();
            let dir = t.path().to_path_buf();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let got: Vec<(MachineKey, bool)> = (0..8)
                .map(|_| {
                    let (dir, barrier) = (dir.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        load_or_create_inner(&dir).unwrap()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect();
            assert!(got.iter().all(|(k, _)| k.id() == got[0].0.id()));
            assert_eq!(got.iter().filter(|(_, won)| *won).count(), 1);
        }
    }

    /// Env var that turns `key_race_child` into a racer for `home`.
    const RACE_HOME: &str = "C9_TEST_MACHINE_KEY_RACE_HOME";
    /// The racer waits for this file to exist, so both start together.
    const RACE_GO: &str = "go";

    /// Not a test on its own: a child process of
    /// `two_processes_creating_the_key_end_with_one_key`.
    #[test]
    fn key_race_child() {
        let Some(home) = std::env::var_os(RACE_HOME) else {
            return;
        };
        let home = PathBuf::from(home);
        let go = home.join(RACE_GO);
        // A parent that died before writing `go` must not leave us spinning.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !go.exists() {
            assert!(
                home.exists() && std::time::Instant::now() < deadline,
                "no go signal from the parent"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let (key, won) = load_or_create_inner(&home).unwrap();
        println!("MACHINE_ID={} WON={}", key.id(), won);
    }

    #[test]
    fn two_processes_creating_the_key_end_with_one_key() {
        let t = home();
        let exe = std::env::current_exe().unwrap();
        let children: Vec<_> = (0..2)
            .map(|_| {
                Command::new(&exe)
                    .args([
                        "--exact",
                        "engine::machine::tests::key_race_child",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env(RACE_HOME, t.path())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        // Give both children time to reach the wait loop, then start them.
        std::thread::sleep(std::time::Duration::from_millis(300));
        std::fs::write(t.path().join(RACE_GO), b"").unwrap();

        let results: Vec<(String, bool)> = children
            .into_iter()
            .map(|c| {
                let out = c.wait_with_output().unwrap();
                assert!(out.status.success());
                let stdout = String::from_utf8(out.stdout).unwrap();
                let line = stdout
                    .lines()
                    .find_map(|l| l.split_once("MACHINE_ID=").map(|(_, rest)| rest))
                    .unwrap_or_else(|| panic!("no id in child output: {}", stdout));
                let (id, won) = line.split_once(" WON=").unwrap();
                (id.to_string(), won == "true")
            })
            .collect();
        let ids: Vec<&str> = results.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids[0], ids[1]);
        assert_eq!(results.iter().filter(|(_, won)| *won).count(), 1);
        // The winner's key is the one on disk.
        std::fs::remove_file(t.path().join(RACE_GO)).unwrap();
        assert_eq!(load_or_create(t.path()).unwrap().id(), ids[0]);
    }
}
