//! Shared cache-consistency rules for provider-specific transcript readers.
//!
//! The parsers intentionally remain provider-specific.  They do, however,
//! need the same conservative rule before treating metadata equality as proof
//! that a cached transcript summary is still current.

use std::fs;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Reads non-empty JSONL lines starting at `offset` bytes into the file, up
/// to EOF. Returns the lines plus the byte offset immediately after the last
/// *complete* line consumed.
///
/// A trailing line with no terminating newline (the file was read mid-write)
/// is left unconsumed rather than parsed: its bytes are not counted in the
/// returned offset, so the next call re-reads it from the start once it's
/// actually complete, instead of an incremental reader silently resuming
/// from the middle of a line.
///
/// Shared by any incremental, offset-tracking file cache (subagent
/// detection, message counting); a full read is just `offset: 0`.
pub(crate) fn read_lines_from_offset(path: &Path, offset: u64) -> io::Result<(Vec<String>, u64)> {
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut consumed = offset;
    let mut buf = String::new();
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            break;
        }
        if !buf.ends_with('\n') {
            // Incomplete trailing line — stop without consuming it.
            break;
        }
        consumed += n as u64;
        let trimmed = buf.trim_end_matches(['\n', '\r']);
        if !trimmed.trim().is_empty() {
            lines.push(trimmed.to_string());
        }
    }
    Ok((lines, consumed))
}

/// Provider-neutral metadata version used by file-backed caches.
///
/// The parsers keep their own cache entries and parsing semantics, but they
/// now consume the same file-version primitive. `identity` is the Unix inode
/// when available and zero on platforms without a replacement identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileVersion {
    pub(crate) len: u64,
    pub(crate) modified_nanos: u128,
    pub(crate) changed_nanos: u128,
    pub(crate) identity: u64,
}

impl FileVersion {
    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(path)?;
        Self::from_metadata(&metadata)
    }

    pub(crate) fn from_metadata(metadata: &fs::Metadata) -> io::Result<Self> {
        let modified_nanos = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let changed_nanos = metadata
                .ctime()
                .saturating_mul(1_000_000_000)
                .saturating_add(metadata.ctime_nsec() as i64)
                .max(0) as u128;
            return Ok(Self {
                len: metadata.len(),
                modified_nanos,
                changed_nanos,
                identity: metadata.ino(),
            });
        }

        #[cfg(not(unix))]
        Ok(Self {
            len: metadata.len(),
            modified_nanos,
            changed_nanos: 0,
            identity: 0,
        })
    }

    pub(crate) fn supports_unchanged_fast_path(self) -> bool {
        has_strong_file_stamp(self.changed_nanos, self.identity)
    }
}

/// Return whether a file stamp is strong enough for an unchanged fast path.
///
/// A non-zero inode/file identity distinguishes replacement, while a
/// sub-second change time avoids trusting filesystems whose timestamps only
/// advance once per second.  Callers must content-validate or fully parse when
/// this returns false.
pub(crate) fn has_strong_file_stamp(changed_nanos: u128, file_id: u64) -> bool {
    file_id != 0 && changed_nanos != 0 && changed_nanos % 1_000_000_000 != 0
}

#[cfg(test)]
mod tests {
    use super::{has_strong_file_stamp, FileVersion};

    #[test]
    fn rejects_missing_or_seconds_only_metadata() {
        assert!(!has_strong_file_stamp(0, 1));
        assert!(!has_strong_file_stamp(1_000_000_000, 1));
        assert!(!has_strong_file_stamp(1_000_000_001, 0));
    }

    #[test]
    fn accepts_subsecond_change_time_with_identity() {
        assert!(has_strong_file_stamp(1_000_000_001, 42));
    }

    #[test]
    fn file_version_reads_shared_metadata_shape() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("synthetic.jsonl");
        std::fs::write(&path, b"one\ntwo\n").unwrap();

        let version = FileVersion::read(&path).unwrap();

        assert_eq!(version.len, 8);
        assert!(version.modified_nanos > 0);
        #[cfg(unix)]
        assert!(version.identity > 0);
    }
}
