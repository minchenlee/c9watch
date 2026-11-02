//! Paths of the files one process has open.
//!
//! Used as liveness evidence: a Codex or Cursor Agent CLI process holds its
//! transcript open while the session is loaded. Callers must treat every
//! error as "no evidence".
//!
//! macOS reads the descriptor table with `proc_pidinfo(PROC_PIDLISTFDS)` and
//! each vnode path with `proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO)`; this is
//! the only `unsafe` code. Linux reads the `/proc/<pid>/fd` symlinks. The
//! `libproc` crate was not used: it has no vnode-path type and its build
//! script needs bindgen and libclang.

use std::io;
use std::path::{Path, PathBuf};

/// Paths of the regular files (vnodes) that `pid` has open.
pub(crate) fn open_file_paths(pid: u32) -> io::Result<Vec<PathBuf>> {
    #[cfg(target_os = "macos")]
    {
        macos::open_file_paths(pid)
    }
    #[cfg(target_os = "linux")]
    {
        paths_from_fd_dir(&Path::new("/proc").join(pid.to_string()).join("fd"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// Targets of the descriptor symlinks in a Linux `/proc/<pid>/fd` directory.
/// A descriptor closed while the directory is read is skipped. Non-file
/// targets (`socket:[…]`, `pipe:[…]`, `anon_inode:…`) are not absolute paths
/// and are left out.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn paths_from_fd_dir(fd_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(fd_dir)? {
        let entry = entry?;
        match std::fs::read_link(entry.path()) {
            Ok(target) if target.is_absolute() => paths.push(target),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(paths)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{CStr, OsStr};
    use std::io;
    use std::mem;
    use std::os::raw::c_int;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    // <sys/proc_info.h>. libc does not define the vnode-path flavor.
    const PROC_PIDLISTFDS: c_int = 1;
    const PROC_PIDFDVNODEPATHINFO: c_int = 2;
    const PROX_FDTYPE_VNODE: u32 = 1;
    const MAXPATHLEN: usize = 1024;

    /// `struct vnode_fdinfowithpath`: `proc_fileinfo` (24 bytes) and
    /// `vnode_info` (152 bytes), then `vip_path[MAXPATHLEN]`. Only the path is
    /// read. Size 1200, path offset 176, alignment 8 (checked against the
    /// macOS 26 SDK header with `sizeof`/`offsetof`).
    #[repr(C)]
    struct VnodeFdInfoWithPath {
        _fileinfo: [u64; 3],
        _vnode: [u64; 19],
        path: [u8; MAXPATHLEN],
    }

    pub(super) fn open_file_paths(pid: u32) -> io::Result<Vec<PathBuf>> {
        let pid = c_int::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut paths = Vec::new();
        for fd in list_fds(pid)? {
            if fd.proc_fdtype != PROX_FDTYPE_VNODE {
                continue;
            }
            if let Some(path) = vnode_path(pid, fd.proc_fd)? {
                paths.push(path);
            }
        }
        Ok(paths)
    }

    fn list_fds(pid: c_int) -> io::Result<Vec<libc::proc_fdinfo>> {
        let entry_size = mem::size_of::<libc::proc_fdinfo>();
        // SAFETY: a null buffer of size 0 asks only for the needed size.
        let needed =
            unsafe { libc::proc_pidinfo(pid, PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return Err(io::Error::last_os_error());
        }
        // Room for descriptors opened between the two calls.
        let capacity = needed as usize / entry_size + 32;
        let mut fds = vec![
            libc::proc_fdinfo {
                proc_fd: 0,
                proc_fdtype: 0,
            };
            capacity
        ];
        let buffer_size = c_int::try_from(capacity * entry_size)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: the buffer holds `capacity` entries, and `buffer_size` is
        // its exact size in bytes; the kernel writes at most that many.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr().cast(),
                buffer_size,
            )
        };
        if written <= 0 {
            return Err(io::Error::last_os_error());
        }
        fds.truncate(written as usize / entry_size);
        Ok(fds)
    }

    /// The path of one vnode descriptor. `None` when it was closed after the
    /// table was listed (`EBADF`) or the vnode has no path, as for an
    /// unlinked file (`ENOENT`; seen on the codex app-server daemon).
    fn vnode_path(pid: c_int, fd: i32) -> io::Result<Option<PathBuf>> {
        let size = mem::size_of::<VnodeFdInfoWithPath>();
        let mut info = mem::MaybeUninit::<VnodeFdInfoWithPath>::zeroed();
        // SAFETY: `info` is a zeroed, writable buffer of exactly `size` bytes
        // laid out like the kernel struct for this flavor.
        let written = unsafe {
            libc::proc_pidfdinfo(
                pid,
                fd,
                PROC_PIDFDVNODEPATHINFO,
                info.as_mut_ptr().cast(),
                size as c_int,
            )
        };
        if written <= 0 {
            let error = io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::EBADF | libc::ENOENT)) {
                return Ok(None);
            }
            return Err(error);
        }
        if written as usize != size {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        // SAFETY: the buffer was zero-initialized and the kernel filled all
        // `size` bytes; every field is plain bytes.
        let info = unsafe { info.assume_init() };
        let path = CStr::from_bytes_until_nul(&info.path)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        let path = OsStr::from_bytes(path.to_bytes());
        Ok((!path.is_empty()).then(|| PathBuf::from(path)))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn struct_layout_matches_the_sdk_header() {
            assert_eq!(mem::size_of::<VnodeFdInfoWithPath>(), 1200);
            assert_eq!(mem::align_of::<VnodeFdInfoWithPath>(), 8);
            assert_eq!(mem::offset_of!(VnodeFdInfoWithPath, path), 176);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn lists_a_file_this_process_has_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("held-open.jsonl");
        let _file = std::fs::File::create(&path).unwrap();
        let expected = path.canonicalize().unwrap();
        let paths = open_file_paths(std::process::id()).unwrap();
        assert!(
            paths
                .iter()
                .any(|open| open.canonicalize().ok().as_ref() == Some(&expected)),
            "{expected:?} not in {paths:?}"
        );
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn unlinked_open_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unlinked.tmp");
        let _file = std::fs::File::create(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(open_file_paths(std::process::id()).is_ok());
    }

    #[test]
    fn missing_process_is_an_error() {
        // pid_max is far below this on macOS and Linux.
        assert!(open_file_paths(i32::MAX as u32).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn proc_fd_dir_symlinks_become_paths() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        symlink(
            "/home/u/.codex/sessions/rollout-a.jsonl",
            dir.path().join("3"),
        )
        .unwrap();
        symlink("socket:[12345]", dir.path().join("4")).unwrap();
        symlink("pipe:[6789]", dir.path().join("5")).unwrap();
        symlink("/dev/null", dir.path().join("0")).unwrap();
        let mut paths = paths_from_fd_dir(dir.path()).unwrap();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/dev/null"),
                PathBuf::from("/home/u/.codex/sessions/rollout-a.jsonl"),
            ]
        );
    }

    #[test]
    fn unreadable_proc_fd_dir_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(paths_from_fd_dir(&dir.path().join("gone")).is_err());
    }
}
