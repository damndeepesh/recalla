//! Immutable, content-addressed zstd copies of raw session bytes.
//!
//! Each ingested byte range of a log becomes `<dir>/<blake3>.zst`, where the
//! hash is over the RAW bytes. Files are write-once (temp file, fsync,
//! rename) and 0600. The archive holds unredacted bytes by design: it is a
//! local copy of data already on disk (see the ingest plan / SPEC).

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use crate::paths;
use crate::store::Store;

/// zstd level: fast with a good ratio on JSONL.
const LEVEL: i32 = 3;

/// A segment in the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// Hex blake3 of the raw bytes; the file is `<hash>.zst`.
    pub hash: String,
    /// Size of the compressed file on disk.
    pub compressed_bytes: u64,
    /// The file already existed (identical content), nothing was written.
    pub existed: bool,
}

fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_owned())
}

/// Compresses `bytes` exactly as [`write_segment`] stores them (also used
/// by dry runs to report archive size without writing).
///
/// # Errors
/// zstd encoder errors.
pub fn compress(bytes: &[u8]) -> io::Result<Vec<u8>> {
    zstd::encode_all(bytes, LEVEL)
}

/// Stores `bytes` as `<dir>/<blake3>.zst`, creating `dir` (0700) if needed.
/// Idempotent: an existing file with that hash is reused.
///
/// # Errors
/// `InvalidInput` for empty `bytes` (empty segments are never archived);
/// I/O errors from writing.
pub fn write_segment(dir: &Path, bytes: &[u8]) -> io::Result<Written> {
    if bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty archive segment",
        ));
    }
    let hash = blake3::hash(bytes).to_hex().to_string();
    let path = dir.join(format!("{hash}.zst"));
    if let Ok(meta) = fs::metadata(&path) {
        return Ok(Written {
            hash,
            compressed_bytes: meta.len(),
            existed: true,
        });
    }
    paths::ensure_dir(dir)?;
    let compressed = compress(bytes)?;
    let tmp = dir.join(format!("{hash}.zst.tmp-{}", std::process::id()));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let result = (|| {
        let mut file = opts.open(&tmp)?;
        paths::restrict_file(&tmp)?;
        file.write_all(&compressed)?;
        file.sync_all()?;
        fs::rename(&tmp, &path)?;
        // Persist the rename itself (best effort is not enough for an archive).
        #[cfg(unix)]
        File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        // Leave no stray temp file; the original error is what matters.
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(Written {
        hash,
        compressed_bytes: compressed.len() as u64,
        existed: false,
    })
}

/// Reads one segment back and verifies its hash.
///
/// # Errors
/// `InvalidData` for a malformed hash or content that doesn't match it;
/// I/O and decode errors.
pub fn read_segment(dir: &Path, hash: &str) -> io::Result<Vec<u8>> {
    if !is_hash(hash) {
        return Err(invalid("malformed archive segment hash"));
    }
    let raw = zstd::decode_all(File::open(dir.join(format!("{hash}.zst")))?)?;
    if blake3::hash(&raw).to_hex().as_str() != hash {
        return Err(invalid("archive segment does not match its hash"));
    }
    Ok(raw)
}

/// Reconstructs the archived bytes of a source by concatenating its segments
/// in start order. Every segment is hash-verified and the ranges must be
/// contiguous from offset 0.
///
/// # Errors
/// `InvalidData` on a gap, overlap, wrong length or hash mismatch; store and
/// I/O errors.
pub fn read_source(store: &Store, dir: &Path, source_id: i64) -> io::Result<Vec<u8>> {
    let segments = store.segments(source_id).map_err(io::Error::other)?;
    let mut out = Vec::new();
    for seg in segments {
        if seg.start != out.len() as u64 {
            return Err(invalid("archive segments are not contiguous"));
        }
        let raw = read_segment(dir, &seg.hash)?;
        if seg.end - seg.start != raw.len() as u64 {
            return Err(invalid("archive segment length does not match its range"));
        }
        out.extend_from_slice(&raw);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Checkpoint, SegmentRow};

    #[test]
    fn write_is_idempotent_and_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("archive");
        let data = b"{\"a\":1}\n".repeat(1000);
        let w = write_segment(&dir, &data).unwrap();
        assert!(!w.existed);
        assert!(w.compressed_bytes < data.len() as u64 / 5);
        let again = write_segment(&dir, &data).unwrap();
        assert!(again.existed);
        assert_eq!(again.hash, w.hash);
        assert_eq!(read_segment(&dir, &w.hash).unwrap(), data);
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, [format!("{}.zst", w.hash)]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir.join(&names[0])), 0o600);
            assert_eq!(mode(&dir), 0o700);
        }
    }

    #[test]
    fn empty_segment_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let err = write_segment(tmp.path(), b"").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn corrupt_or_bad_hash_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let w = write_segment(tmp.path(), b"original\n").unwrap();
        let other = zstd::encode_all(&b"tampered\n"[..], LEVEL).unwrap();
        fs::write(tmp.path().join(format!("{}.zst", w.hash)), other).unwrap();
        let kind = |r: io::Result<Vec<u8>>| r.unwrap_err().kind();
        assert_eq!(
            kind(read_segment(tmp.path(), &w.hash)),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            kind(read_segment(tmp.path(), "../etc/passwd")),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn read_source_concatenates_and_checks_contiguity() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = Store::open_in_memory().unwrap();
        let id = s.upsert_source("h", "/f").unwrap();
        let (a, b) = (b"line one\n".as_slice(), b"line two\n".as_slice());
        let cp = |n: u64| Checkpoint {
            identity: "i".into(),
            tail_hash: "t".into(),
            size: n,
            next_offset: n,
        };
        for (start, bytes) in [(0u64, a), (a.len() as u64, b)] {
            let w = write_segment(tmp.path(), bytes).unwrap();
            let end = start + bytes.len() as u64;
            let seg = SegmentRow {
                start,
                end,
                hash: w.hash,
            };
            s.ingest_batch(id, &[], &cp(end), Some(&seg), false)
                .unwrap();
        }
        assert_eq!(read_source(&s, tmp.path(), id).unwrap(), [a, b].concat());

        // A gap is an error, not a silently short file.
        let id2 = s.upsert_source("h", "/g").unwrap();
        let w = write_segment(tmp.path(), b).unwrap();
        let seg = SegmentRow {
            start: 5,
            end: 5 + b.len() as u64,
            hash: w.hash,
        };
        s.ingest_batch(id2, &[], &cp(20), Some(&seg), false)
            .unwrap();
        assert_eq!(
            read_source(&s, tmp.path(), id2).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
