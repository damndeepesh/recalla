//! Per-agent adapters: locate session logs and decode them into normalized
//! [`Event`]s with exact provenance. Pure read-only I/O; no redaction or
//! storage happens here (the ingest pipeline does that).
//!
//! Privacy: parsers report counts in [`ParseStats`] and never log content.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

pub mod claude;

/// Largest text a stored event may carry. Ingest applies [`truncate_middle`]
/// AFTER redaction; the harness emits full text so a cut can't split a secret.
pub const MAX_EVENT_BYTES: usize = 32 * 1024;

/// What an event records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    UserText,
    AssistantText,
    /// `text` holds the compact JSON of the tool input.
    ToolCall {
        id: String,
        name: String,
    },
    /// `text` holds the joined text parts of the result.
    ToolResult {
        tool_use_id: String,
        is_error: bool,
    },
    /// A compaction summary written by the agent.
    Summary,
}

/// One normalized, provenance-linked piece of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub harness: &'static str,
    pub session_id: String,
    pub message_uuid: Option<String>,
    pub parent_uuid: Option<String>,
    pub source_path: PathBuf,
    /// 1-based line number in `source_path`.
    pub line: u64,
    /// Offset of the start of the line, usable to resume parsing.
    pub byte_offset: u64,
    /// Unix milliseconds.
    pub timestamp_ms: Option<i64>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub sidechain: bool,
    pub kind: EventKind,
    pub text: String,
}

/// A session log found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFile {
    pub path: PathBuf,
    pub project_dir_name: String,
    pub is_subagent: bool,
}

/// Counts from one parse run. Never contains content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseStats {
    /// Complete lines consumed.
    pub lines: u64,
    pub events: u64,
    /// Invalid UTF-8, invalid JSON, or not a JSON object.
    pub malformed_lines: u64,
    /// Records dropped, by `type` (`isMeta` user records count as `user(meta)`).
    pub skipped_record_types: BTreeMap<String, u64>,
    /// Blocks dropped, by block `type`.
    pub skipped_blocks: BTreeMap<String, u64>,
    /// The file ended mid-line; that line was left unconsumed.
    pub partial_tail: bool,
}

/// An agent whose session logs recalla can read.
pub trait Harness {
    fn name(&self) -> &'static str;

    /// # Errors
    /// I/O errors other than a missing root (which yields an empty list).
    fn discover(&self, root: &Path) -> io::Result<Vec<SessionFile>>;

    /// Parse complete lines from `from_offset`; returns the events, stats and
    /// the offset to resume from.
    ///
    /// # Errors
    /// I/O errors, or `from_offset` past the end of the file.
    fn parse(&self, path: &Path, from_offset: u64) -> io::Result<(Vec<Event>, ParseStats, u64)>;

    /// Parse complete lines of `bytes`, which begin at file offset
    /// `base_offset` (a line start) after `base_line` lines. Returns the
    /// events, stats and the number of bytes consumed.
    fn parse_bytes(
        &self,
        bytes: &[u8],
        base_offset: u64,
        base_line: u64,
        path: &Path,
    ) -> (Vec<Event>, ParseStats, u64);
}

/// Count `\n` in the first `limit` bytes read from `reader`, leaving it
/// positioned there.
///
/// # Errors
/// I/O errors from reading.
#[allow(clippy::naive_bytecount)] // a dependency isn't worth it here
pub fn count_newlines(reader: impl io::Read, limit: u64) -> io::Result<u64> {
    let mut reader = io::BufReader::new(reader.take(limit));
    let mut count = 0;
    loop {
        let chunk = io::BufRead::fill_buf(&mut reader)?;
        if chunk.is_empty() {
            return Ok(count);
        }
        count += chunk.iter().filter(|&&b| b == b'\n').count() as u64;
        let n = chunk.len();
        io::BufRead::consume(&mut reader, n);
    }
}

/// Keep the head and tail of `text` when it exceeds `max_bytes`, joined by an
/// omission marker. Cuts land on char boundaries. Returns the text and whether
/// it was truncated.
#[must_use]
pub fn truncate_middle(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut head = max_bytes / 2;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - (max_bytes - max_bytes / 2);
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    let omitted = tail - head;
    (
        format!(
            "{}\n[… {omitted} bytes omitted …]\n{}",
            &text[..head],
            &text[tail..]
        ),
        true,
    )
}

/// Parse `YYYY-MM-DDTHH:MM:SS(.fff)?(Z|±HH:MM)` into unix milliseconds.
/// Fractions beyond milliseconds are truncated. Anything else gives `None`.
#[must_use]
pub fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || !s.is_ascii() {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let sep = |i: usize, c: u8| (b[i] == c).then_some(());
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    sep(4, b'-')?;
    sep(7, b'-')?;
    sep(10, b'T')?;
    let (hour, min, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    sep(13, b':')?;
    sep(16, b':')?;
    let mut i = 19;
    let mut millis = 0;
    if b[i] == b'.' {
        let start = i + 1;
        i = start;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None;
        }
        let frac = &s[start..i.min(start + 3)];
        millis = frac.parse::<i64>().ok()? * 10_i64.pow(u32::try_from(3 - frac.len()).ok()?);
    }
    let offset_secs = match &s[i..] {
        "Z" => 0,
        z if z.len() == 6 && (z.starts_with('+') || z.starts_with('-')) => {
            sep(i + 3, b':')?;
            let (oh, om) = (num(i + 1..i + 3)?, num(i + 4..i + 6)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let secs = oh * 3600 + om * 60;
            if z.starts_with('-') { -secs } else { secs }
        }
        _ => return None,
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let dim = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if !(1..=dim).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3600 + min * 60 + sec;
    Some((secs - offset_secs) * 1000 + millis)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_known_values() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ms("2025-01-02T03:04:05.678Z"),
            Some(1_735_787_045_678)
        );
        // 2000-02-29 exists (leap), fraction padded and truncated
        assert_eq!(
            parse_rfc3339_ms("2000-02-29T00:00:00.5Z"),
            Some(951_782_400_500)
        );
        assert_eq!(
            parse_rfc3339_ms("2000-02-29T00:00:00.123456Z"),
            Some(951_782_400_123)
        );
    }

    #[test]
    fn rfc3339_offsets() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_rfc3339_ms("1969-12-31T19:00:00-05:00"), Some(0));
        assert_eq!(parse_rfc3339_ms("1969-12-31T23:59:59Z"), Some(-1000));
    }

    #[test]
    fn rfc3339_rejects_invalid() {
        for bad in [
            "",
            "2025-01-02",
            "2025-01-02T03:04:05",
            "2025-02-30T00:00:00Z",
            "2023-02-29T00:00:00Z",
            "2025-13-01T00:00:00Z",
            "2025-01-02T24:00:00Z",
            "2025-01-02T03:04:05.Z",
            "2025-01-02T03:04:05+0100",
            "2025-01-02T03:04:05+25:00",
            "2025-01-02 03:04:05Z",
            "2025-01-02T03:04:05Zjunk",
            "2025-01-02T03:04:05日Z",
        ] {
            assert_eq!(parse_rfc3339_ms(bad), None, "{bad}");
        }
    }

    #[test]
    fn truncate_noop_when_short() {
        assert_eq!(truncate_middle("abc", 3), ("abc".to_owned(), false));
    }

    #[test]
    fn truncate_keeps_head_tail_and_char_boundaries() {
        let text = "日本語".repeat(100); // 900 bytes, 3-byte chars
        for max in [7, 10, 100, 301] {
            let (out, cut) = truncate_middle(&text, max);
            assert!(cut);
            assert!(out.contains("bytes omitted"));
            assert!(out.starts_with('日') && out.ends_with('語'));
            assert!(out.len() <= max + 40);
        }
    }

    #[test]
    fn truncate_reports_omitted_count() {
        let (out, _) = truncate_middle(&"a".repeat(100), 20);
        assert_eq!(
            out,
            format!(
                "{}\n[… 80 bytes omitted …]\n{}",
                "a".repeat(10),
                "a".repeat(10)
            )
        );
    }
}
