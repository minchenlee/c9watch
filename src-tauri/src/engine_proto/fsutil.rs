//! Private directory creation that never follows a symlink.
//!
//! Ported from `cli/pm_fs.rs::ensure_private_dir`, which still runs
//! `create_dir_all` on the parent and so follows a symlink anywhere above
//! the leaf. Here every component is checked with `fstatat` (no follow) and
//! opened with `openat(O_NOFOLLOW|O_DIRECTORY)` relative to the parent fd,
//! so a link swapped in between the check and the open is caught too.

use std::ffi::{CString, OsStr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

/// Create `dir` and any missing parents, then make `dir` a 0700 directory
/// owned by us.
///
/// - A symlink at any level is rejected. The one exception is a root-owned
///   link in a root-owned directory above the leaf, such as `/var` and
///   `/tmp` on macOS: only root can plant or change those.
/// - Missing parents are created 0700. Existing parents keep their mode,
///   but each one must be owned by our euid or root, and must not be group-
///   or world-writable unless it is sticky (like `/tmp`). Otherwise another
///   user could swap the leaf for a link after this check, and every later
///   open by path would follow it.
/// - The leaf must be owned by our euid. Its chmod runs on the open fd, so
///   it can never land on the target of a link.
pub fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    open_private_dir(dir).map(drop)
}

/// `ensure_private_dir`, returning the open fd of the leaf.
pub fn open_private_dir(dir: &Path) -> Result<OwnedFd, String> {
    if !dir.is_absolute() {
        return Err(format!("{:?} is not an absolute path", dir));
    }
    let mut names: Vec<&OsStr> = Vec::new();
    for comp in dir.components() {
        match comp {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(n) => names.push(n),
            Component::ParentDir | Component::Prefix(_) => {
                return Err(format!("{:?} must not contain '..'", dir));
            }
        }
    }

    let root = CString::new("/").expect("no NUL");
    let mut fd =
        openat(None, &root, libc::O_NOFOLLOW).map_err(|e| format!("Failed to open /: {}", e))?;
    let mut at = std::path::PathBuf::from("/");
    let me = unsafe { libc::geteuid() };
    for (i, name) in names.iter().enumerate() {
        let leaf = i + 1 == names.len();
        // `fd` is the parent of `name`: a directory only we or root control.
        check_trusted_parent(&fd, &at, me)?;
        at.push(name);
        let cname =
            CString::new(name.as_bytes()).map_err(|_| format!("{:?} contains a NUL byte", dir))?;
        let symlink_err = || format!("{:?} is a symlink, refusing to use it", at);

        let st = match lstat_at(&fd, &cname) {
            Ok(st) => st,
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {
                if unsafe { libc::mkdirat(fd.as_raw_fd(), cname.as_ptr(), 0o700) } != 0 {
                    let e = std::io::Error::last_os_error();
                    // Lost a race with another creator: use what is there.
                    if e.raw_os_error() != Some(libc::EEXIST) {
                        return Err(format!("Failed to create dir {:?}: {}", at, e));
                    }
                }
                lstat_at(&fd, &cname).map_err(|e| format!("Failed to stat {:?}: {}", at, e))?
            }
            Err(e) => return Err(format!("Failed to stat {:?}: {}", at, e)),
        };

        let follow = match st.st_mode & libc::S_IFMT {
            libc::S_IFDIR => false,
            libc::S_IFLNK => {
                if leaf || st.st_uid != 0 || fstat(&fd).map(|p| p.st_uid).unwrap_or(1) != 0 {
                    return Err(symlink_err());
                }
                true
            }
            _ => return Err(format!("{:?} is not a directory", at)),
        };
        let flags = if follow { 0 } else { libc::O_NOFOLLOW };
        fd = openat(Some(&fd), &cname, flags).map_err(|e| match e.raw_os_error() {
            // A link swapped in after the fstatat above.
            Some(libc::ELOOP) => symlink_err(),
            Some(libc::ENOTDIR) => format!("{:?} is not a directory", at),
            _ => format!("Failed to open dir {:?}: {}", at, e),
        })?;
    }

    let md = fstat(&fd).map_err(|e| format!("Failed to stat dir {:?}: {}", dir, e))?;
    if md.st_uid != me {
        return Err(format!(
            "{:?} is owned by uid {}, not by us (uid {})",
            dir, md.st_uid, me
        ));
    }
    if md.st_mode & 0o777 != 0o700 {
        if unsafe { libc::fchmod(fd.as_raw_fd(), 0o700) } != 0 {
            return Err(format!(
                "Failed to chmod 0700 {:?}: {}",
                dir,
                std::io::Error::last_os_error()
            ));
        }
        let mode = fstat(&fd)
            .map_err(|e| format!("Failed to stat dir {:?}: {}", dir, e))?
            .st_mode
            & 0o777;
        if mode != 0o700 {
            return Err(format!("{:?} is {:o} after chmod 0700", dir, mode));
        }
    }
    Ok(fd)
}

/// Fail unless the open directory `fd` (at path `at`) is owned by `me` or
/// root, and is not group- or world-writable without the sticky bit.
fn check_trusted_parent(fd: &OwnedFd, at: &Path, me: libc::uid_t) -> Result<(), String> {
    let st = fstat(fd).map_err(|e| format!("Failed to stat dir {:?}: {}", at, e))?;
    if st.st_uid != me && st.st_uid != 0 {
        return Err(format!(
            "{:?} is owned by uid {}, not by us (uid {}) or root",
            at, st.st_uid, me
        ));
    }
    let mode = st.st_mode as u32;
    if mode & 0o022 != 0 && mode & libc::S_ISVTX as u32 == 0 {
        return Err(format!(
            "{:?} is writable by others ({:o}) and not sticky",
            at,
            mode & 0o7777
        ));
    }
    Ok(())
}

/// `openat(parent, name, O_RDONLY|O_DIRECTORY|O_CLOEXEC|extra)`. `None`
/// resolves `name` against the cwd (only used for the absolute `/`).
fn openat(
    parent: Option<&OwnedFd>,
    name: &CString,
    extra: libc::c_int,
) -> std::io::Result<OwnedFd> {
    let dirfd = parent.map_or(libc::AT_FDCWD, |p| p.as_raw_fd());
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | extra;
    let fd = unsafe { libc::openat(dirfd, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn lstat_at(parent: &OwnedFd, name: &CString) -> std::io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(st)
}

fn fstat(fd: &OwnedFd) -> std::io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(st)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    // `tempfile` lives under $TMPDIR. On macOS that is below the root-owned
    // `/var -> private/var` link, so every test here also covers the
    // trusted system-link exception.
    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn mode_of(p: &Path) -> u32 {
        fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn creates_missing_dirs_0700() {
        let t = tmp();
        let dir = t.path().join("a").join("b").join("engine");
        ensure_private_dir(&dir).unwrap();
        assert!(dir.is_dir());
        assert_eq!(mode_of(&dir), 0o700);
        // Idempotent.
        ensure_private_dir(&dir).unwrap();
    }

    #[test]
    fn existing_leaf_is_chmodded_to_0700() {
        let t = tmp();
        let dir = t.path().join("engine");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let parent_mode = mode_of(t.path());
        ensure_private_dir(&dir).unwrap();
        assert_eq!(mode_of(&dir), 0o700);
        // Existing parents keep their mode.
        assert_eq!(mode_of(t.path()), parent_mode);
    }

    #[test]
    fn symlinked_parent_is_rejected() {
        let t = tmp();
        let real = t.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = t.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let err = ensure_private_dir(&link.join("engine")).unwrap_err();
        assert!(err.contains("symlink"), "{}", err);
        // Nothing was created through the link.
        assert!(!real.join("engine").exists());
    }

    #[test]
    fn symlinked_leaf_is_rejected_and_target_untouched() {
        let t = tmp();
        let real = t.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        let link = t.path().join("engine");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let err = ensure_private_dir(&link).unwrap_err();
        assert!(err.contains("symlink"), "{}", err);
        assert_eq!(mode_of(&real), 0o755);
    }

    #[test]
    fn dangling_symlink_leaf_is_rejected() {
        let t = tmp();
        let link = t.path().join("engine");
        std::os::unix::fs::symlink(t.path().join("missing"), &link).unwrap();
        assert!(ensure_private_dir(&link).unwrap_err().contains("symlink"));
        assert!(!t.path().join("missing").exists());
    }

    #[test]
    fn file_in_the_path_is_rejected() {
        let t = tmp();
        let file = t.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert!(ensure_private_dir(&file)
            .unwrap_err()
            .contains("not a directory"));
        assert!(ensure_private_dir(&file.join("engine"))
            .unwrap_err()
            .contains("not a directory"));
    }

    #[test]
    fn parent_writable_by_others_is_rejected_unless_sticky() {
        let t = tmp();
        let shared = t.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
        let err = ensure_private_dir(&shared.join("engine")).unwrap_err();
        assert!(err.contains("writable by others"), "{}", err);
        assert!(!shared.join("engine").exists());

        // Group-writable is no better.
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(ensure_private_dir(&shared.join("engine")).is_err());

        // Sticky, like /tmp: others cannot rename our entries.
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).unwrap();
        ensure_private_dir(&shared.join("engine")).unwrap();
        assert_eq!(mode_of(&shared.join("engine")), 0o700);
    }

    #[test]
    fn relative_and_dotdot_paths_are_rejected() {
        assert!(ensure_private_dir(Path::new("rel/engine")).is_err());
        let t = tmp();
        assert!(ensure_private_dir(&t.path().join("a/../engine")).is_err());
    }
}
