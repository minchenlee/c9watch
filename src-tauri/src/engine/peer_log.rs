//! Bounded, incremental JSONL mail tailer. No eligible mail means no open.
use super::transcript::{mail_fact, MailFact};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
pub const MAX_LINE: usize = 1024 * 1024;
#[derive(Debug, Default)]
pub struct Tailer {
    path: PathBuf,
    inode: Option<(u64, u64)>,
    offset: u64,
    partial: Vec<u8>,
    skipping: bool,
}
impl Tailer {
    pub fn at(path: &Path, offset: u64) -> Self {
        Self {
            path: path.into(),
            offset,
            ..Default::default()
        }
    }
    pub fn read(&mut self, path: &Path, eligible: bool) -> std::io::Result<Vec<MailFact>> {
        if !eligible {
            return Ok(vec![]);
        }
        if self.path != path {
            *self = Self::at(path, 0);
        }
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let m = f.metadata()?;
        if !m.is_file() {
            return Ok(vec![]);
        }
        let inode = (m.dev(), m.ino());
        if self.inode.is_some_and(|i| i != inode) || m.len() < self.offset {
            self.offset = 0;
            self.partial.clear();
            self.skipping = false;
        }
        self.inode = Some(inode);
        f.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = vec![];
        f.take(MAX_LINE as u64).read_to_end(&mut bytes)?;
        self.offset += bytes.len() as u64;
        let mut out = vec![];
        for b in bytes {
            if b == b'\n' {
                if !self.skipping {
                    if let Ok(v) = serde_json::from_slice(&self.partial) {
                        out.extend(mail_fact(&v));
                    }
                }
                self.partial.clear();
                self.skipping = false;
            } else if !self.skipping {
                if self.partial.len() == MAX_LINE {
                    self.partial.clear();
                    self.skipping = true;
                    eprintln!("[engine] skipped oversized CC JSONL line");
                } else {
                    self.partial.push(b);
                }
            }
        }
        Ok(out)
    }
}
/// Capture before starting the writer, including entries flushed before tick 1.
pub fn offset(path: &Path) -> u64 {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .and_then(|f| f.metadata())
        .map(|m| m.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn offsets_partial_lines_oversize_rotation_and_ineligible_no_open() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("session.jsonl");
        let entry = r#"{"type":"user","turnOrigin":"peer","origin":{"kind":"peer","msg_id":"wire"},"uuid":"u"}"#;
        std::fs::write(&p, format!("{}\n", entry)).unwrap();
        let mut tail = Tailer::at(&p, offset(&p));
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(&entry.as_bytes()[..20]).unwrap();
        assert!(tail.read(&p, true).unwrap().is_empty());
        f.write_all(&entry.as_bytes()[20..]).unwrap();
        f.write_all(b"\n").unwrap();
        assert_eq!(tail.read(&p, true).unwrap().len(), 1);
        f.write_all(&vec![b'x'; MAX_LINE + 20]).unwrap();
        f.write_all(format!("\n{}\n", entry).as_bytes()).unwrap();
        assert!(tail.read(&p, true).unwrap().is_empty());
        assert_eq!(tail.read(&p, true).unwrap().len(), 1);
        std::fs::write(&p, format!("{}\n", entry)).unwrap();
        assert_eq!(tail.read(&p, true).unwrap().len(), 1);
        let p2 = t.path().join("other.jsonl");
        std::fs::write(&p2, format!("{}\n", entry)).unwrap();
        assert_eq!(tail.read(&p2, true).unwrap().len(), 1);
        assert!(tail
            .read(&t.path().join("missing"), false)
            .unwrap()
            .is_empty());
    }
}
