//! Shared cache-consistency rules for provider-specific transcript readers.
//!
//! The parsers intentionally remain provider-specific.  They do, however,
//! need the same conservative rule before treating metadata equality as proof
//! that a cached transcript summary is still current.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Keep small transcripts exact while avoiding a full-history read on every
/// poll of a large live transcript. These values intentionally match the
/// bounded prefix policy used by the Cursor transcript reader, but the
/// Claude-specific caches own their validation state independently.
pub(crate) const EXACT_PREFIX_VERIFY_LIMIT: u64 = 4 * 1024 * 1024;
pub(crate) const PREFIX_GUARD_BYTES: u64 = 64 * 1024;
pub(crate) const MIN_FULL_VERIFY_GROWTH_BYTES: u64 = 1024 * 1024;
pub(crate) const FULL_VERIFY_GROWTH_DIVISOR: u64 = 8;
pub(crate) const PREFIX_FULL_REVALIDATION_INTERVAL: Duration = Duration::from_secs(60);

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrefixGuard {
    pub(crate) len: u64,
    pub(crate) head_hash: u64,
    pub(crate) tail_hash: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrefixSnapshot {
    pub(crate) hash: u64,
    pub(crate) guard: PrefixGuard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrefixValidationKind {
    Full,
    Guard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrefixValidation {
    pub(crate) snapshot: PrefixSnapshot,
    pub(crate) kind: PrefixValidationKind,
    pub(crate) bytes_read: u64,
}

fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn hash_range(path: &Path, start: u64, end: u64) -> io::Result<(u64, u64)> {
    if end < start {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "prefix range ends before it starts",
        ));
    }
    if fs::metadata(path)?.len() < end {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "file is shorter than the requested prefix range",
        ));
    }

    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut hash = FNV_OFFSET_BASIS;
    let mut remaining = end - start;
    let mut bytes_read = 0;
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        let want = buffer.len().min(remaining as usize);
        let read = file.read(&mut buffer[..want])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "file ended while hashing a prefix range",
            ));
        }
        hash = hash_bytes(hash, &buffer[..read]);
        remaining -= read as u64;
        bytes_read += read as u64;
    }
    Ok((hash, bytes_read))
}

fn hash_prefix_guard(path: &Path, len: u64) -> io::Result<(PrefixGuard, u64)> {
    let head_len = len.min(PREFIX_GUARD_BYTES);
    let (head_hash, head_bytes) = hash_range(path, 0, head_len)?;
    if len <= PREFIX_GUARD_BYTES {
        return Ok((
            PrefixGuard {
                len,
                head_hash,
                tail_hash: head_hash,
            },
            head_bytes,
        ));
    }

    let tail_start = len.saturating_sub(PREFIX_GUARD_BYTES);
    let (tail_hash, tail_bytes) = hash_range(path, tail_start, len)?;
    Ok((
        PrefixGuard {
            len,
            head_hash,
            tail_hash,
        },
        head_bytes + tail_bytes,
    ))
}

fn hash_file_prefix_with_bytes(path: &Path, len: u64) -> io::Result<(PrefixSnapshot, u64)> {
    let (hash, full_bytes) = hash_range(path, 0, len)?;
    let (guard, guard_bytes) = hash_prefix_guard(path, len)?;
    Ok((PrefixSnapshot { hash, guard }, full_bytes + guard_bytes))
}

/// Hash the first `len` bytes and store both an exact prefix hash and the
/// bounded head/tail guard used between full validations.
pub(crate) fn hash_file_prefix(path: &Path, len: u64) -> io::Result<PrefixSnapshot> {
    hash_file_prefix_with_bytes(path, len).map(|(snapshot, _)| snapshot)
}

/// Extend an existing prefix hash over the newly complete bytes and refresh
/// the bounded guard for the new prefix length.
pub(crate) fn extend_prefix_snapshot(
    path: &Path,
    start: u64,
    end: u64,
    previous: PrefixSnapshot,
) -> io::Result<PrefixSnapshot> {
    if end < start {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "prefix range ends before it starts",
        ));
    }
    if fs::metadata(path)?.len() < end {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "file is shorter than the requested prefix range",
        ));
    }

    let suffix_len = end.saturating_sub(start);
    let mut hash = previous.hash;
    if suffix_len > 0 {
        let mut file = fs::File::open(path)?;
        file.seek(SeekFrom::Start(start))?;
        let mut remaining = suffix_len;
        let mut buffer = [0u8; 64 * 1024];
        while remaining > 0 {
            let want = buffer.len().min(remaining as usize);
            let read = file.read(&mut buffer[..want])?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "file ended while extending a prefix hash",
                ));
            }
            hash = hash_bytes(hash, &buffer[..read]);
            remaining -= read as u64;
        }
    }
    let (guard, _) = hash_prefix_guard(path, end)?;
    Ok(PrefixSnapshot { hash, guard })
}

pub(crate) fn next_full_verify_offset(offset: u64) -> u64 {
    if offset <= EXACT_PREFIX_VERIFY_LIMIT {
        return offset;
    }
    let growth = (offset / FULL_VERIFY_GROWTH_DIVISOR).max(MIN_FULL_VERIFY_GROWTH_BYTES);
    offset.saturating_add(growth)
}

/// Validate the cached prefix. Small prefixes and scheduled/deadline checks
/// read the whole cached prefix; otherwise only fixed-size head/tail guards
/// are read. The deadline is checked on lookup, not in a background task, so
/// the contract is "no later than the next lookup after 60 seconds".
pub(crate) fn validate_cached_prefix(
    path: &Path,
    expected: PrefixSnapshot,
    next_full_verify_offset: u64,
    last_full_validation_at: Instant,
    now: Instant,
) -> Option<PrefixValidation> {
    let prefix_len = expected.guard.len;
    let full_validation_due = prefix_len <= EXACT_PREFIX_VERIFY_LIMIT
        || prefix_len >= next_full_verify_offset
        || now.saturating_duration_since(last_full_validation_at)
            >= PREFIX_FULL_REVALIDATION_INTERVAL;

    if full_validation_due {
        let (snapshot, bytes_read) = hash_file_prefix_with_bytes(path, prefix_len).ok()?;
        return (snapshot == expected).then_some(PrefixValidation {
            snapshot,
            kind: PrefixValidationKind::Full,
            bytes_read,
        });
    }

    let (guard, bytes_read) = hash_prefix_guard(path, prefix_len).ok()?;
    (guard == expected.guard).then_some(PrefixValidation {
        snapshot: PrefixSnapshot {
            hash: expected.hash,
            guard,
        },
        kind: PrefixValidationKind::Guard,
        bytes_read,
    })
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

/// Whether a cache may use its append-only incremental path for `current`.
///
/// This is deliberately a conservative metadata gate, not a content proof:
/// a same-inode file that grows can still have been rewritten by a producer
/// that does not honor the append-only contract. Callers must use a full
/// reparse for weak metadata, equal-length observations, or shrink/rewrite
/// cases. In this repository, c9watch's own native-title writer explicitly
/// opens Claude JSONL with `append(true)`, but there is no Claude producer
/// implementation or compact/rewind fixture proving that every Claude live
/// update is append-only. Treat that producer behavior as a bounded contract
/// assumption; this function must not be broadened to treat `len >= offset`
/// as sufficient evidence.
pub(crate) fn can_incrementally_read(previous: FileVersion, current: FileVersion) -> bool {
    previous.supports_unchanged_fast_path()
        && current.supports_unchanged_fast_path()
        && previous.identity != 0
        && previous.identity == current.identity
        && current.len > previous.len
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
    use super::{can_incrementally_read, has_strong_file_stamp, FileVersion};
    use super::{hash_file_prefix, next_full_verify_offset, validate_cached_prefix};
    use std::time::{Duration, Instant};

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
    fn incremental_read_requires_strong_same_file_growth() {
        let previous = FileVersion {
            len: 100,
            modified_nanos: 1,
            changed_nanos: 1_000_000_001,
            identity: 42,
        };
        let grown = FileVersion {
            len: 120,
            modified_nanos: 2,
            changed_nanos: 1_000_000_002,
            identity: 42,
        };
        assert!(can_incrementally_read(previous, grown));

        let same_length = FileVersion { len: 100, ..grown };
        assert!(!can_incrementally_read(previous, same_length));

        let truncated = FileVersion { len: 80, ..grown };
        assert!(!can_incrementally_read(previous, truncated));

        let weak = FileVersion {
            changed_nanos: 2_000_000_000,
            ..grown
        };
        assert!(!can_incrementally_read(previous, weak));
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

    #[test]
    fn large_prefix_guard_validation_reads_bounded_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.jsonl");
        let len = super::EXACT_PREFIX_VERIFY_LIMIT + 256 * 1024;
        std::fs::write(&path, vec![b'x'; len as usize]).unwrap();

        let snapshot = hash_file_prefix(&path, len).unwrap();
        let first_check = Instant::now();
        let validation = validate_cached_prefix(
            &path,
            snapshot,
            next_full_verify_offset(len),
            first_check,
            first_check + Duration::from_secs(1),
        )
        .unwrap();

        assert_eq!(validation.kind, super::PrefixValidationKind::Guard);
        assert!(validation.bytes_read <= super::PREFIX_GUARD_BYTES * 2);
        assert!(validation.bytes_read < len);
    }
}
