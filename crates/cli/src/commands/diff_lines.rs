use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::path::Path;
use std::process::Command;

use autoreview_schema::{AgentFinding, Location, Side};

type Ranges = HashMap<String, Vec<RangeInclusive<u32>>>;

#[derive(Default)]
pub(super) struct ChangedLines {
    old: Ranges,
    new: Ranges,
}

impl ChangedLines {
    pub(super) fn load(repo: &Path, base: &str, head: &str) -> anyhow::Result<Self> {
        let output = Command::new("git")
            .args(["-c", "core.quotePath=false", "diff", "--no-ext-diff", "--no-textconv", "--no-color", "--no-relative", "--find-renames"])
            .args(["--src-prefix=a/", "--dst-prefix=b/", "--unified=0", &format!("{base}...{head}"), "--"])
            .current_dir(repo)
            .output()?;
        anyhow::ensure!(output.status.success(), "could not read changed lines: {}", String::from_utf8_lossy(&output.stderr).trim());
        Self::parse(&output.stdout)
    }

    fn parse(patch: &[u8]) -> anyhow::Result<Self> {
        let mut ranges = Self::default();
        // Source lines can contain arbitrary bytes. Only headers need to
        // be decoded, and filenames are already checked by diff facts.
        let mut path: Option<String> = None;
        let mut old_path: Option<String> = None;
        let mut reading_headers = false;
        for line in patch.split(|byte| *byte == b'\n') {
            if line.starts_with(b"diff --git ") {
                path = None;
                old_path = None;
                reading_headers = true;
            } else if let Some(header) = line.strip_prefix(b"@@ -") {
                reading_headers = false;
                let header = std::str::from_utf8(header)?;
                let mut sides = header.split_ascii_whitespace();
                let old_range = sides.next().ok_or_else(|| anyhow::anyhow!("missing old-side range in Git diff"))?;
                let new_range = sides.next().and_then(|range| range.strip_prefix('+'))
                    .ok_or_else(|| anyhow::anyhow!("invalid new-side range in Git diff"))?;
                insert_range(&mut ranges.old, old_path.as_ref(), old_range)?;
                insert_range(&mut ranges.new, path.as_ref(), new_range)?;
            } else if reading_headers {
                if let Some(new_path) = line.strip_prefix(b"+++ ") {
                    path = decode_path(new_path, b"b/")?;
                }
                if let Some(previous_path) = line.strip_prefix(b"--- ") {
                    old_path = decode_path(previous_path, b"a/")?;
                }
            }
        }
        Ok(ranges)
    }

    pub(super) fn contains_finding(&self, finding: &AgentFinding) -> bool {
        let primary_ranges = match finding.location.side {
            Side::New => &self.new,
            Side::Old => &self.old,
        };
        contains_location(primary_ranges, &finding.location)
            || finding.related_locations.iter().flatten().any(|location| {
                matches!(location.side, Side::New) && contains_location(&self.new, location)
            })
    }
}

fn contains_location(ranges: &Ranges, location: &Location) -> bool {
    let start = location.range.start_line;
    let end = location.range.end_line.unwrap_or(start);
    ranges.get(&location.path).is_some_and(|ranges| {
        ranges.iter().any(|range| *range.start() <= end && *range.end() >= start)
    })
}

fn insert_range(ranges: &mut Ranges, path: Option<&String>, hunk: &str) -> anyhow::Result<()> {
    let Some(path) = path else { return Ok(()) };
    let (start, count) = hunk.split_once(',').unwrap_or((hunk, "1"));
    let start: u32 = start.parse()?;
    let count: u32 = count.parse()?;
    if count > 0 {
        ranges.entry(path.clone()).or_default().push(start..=start.saturating_add(count - 1));
    }
    Ok(())
}

fn decode_path(path: &[u8], prefix: &[u8]) -> anyhow::Result<Option<String>> {
    if path == b"/dev/null" {
        return Ok(None);
    }
    let path = path.strip_suffix(b"\t").unwrap_or(path);
    let mut decoded = Vec::new();
    let path = if let Some(quoted) = path.strip_prefix(b"\"").and_then(|p| p.strip_suffix(b"\"")) {
        let mut bytes = quoted.iter().copied();
        while let Some(byte) = bytes.next() {
            let byte = if byte == b'\\' {
                match bytes.next() {
                    Some(b'\\') => b'\\',
                    Some(b'"') => b'"',
                    Some(b'a') => 7,
                    Some(b'b') => 8,
                    Some(b't') => b'\t',
                    Some(b'n') => b'\n',
                    Some(b'v') => 11,
                    Some(b'f') => 12,
                    Some(b'r') => b'\r',
                    Some(first @ b'0'..=b'3') => {
                        let second = bytes.next().filter(|n| matches!(n, b'0'..=b'7'));
                        let third = bytes.next().filter(|n| matches!(n, b'0'..=b'7'));
                        let (Some(second), Some(third)) = (second, third) else {
                            anyhow::bail!("invalid quoted Git path");
                        };
                        (first - b'0') * 64 + (second - b'0') * 8 + (third - b'0')
                    }
                    _ => anyhow::bail!("invalid quoted Git path"),
                }
            } else {
                byte
            };
            decoded.push(byte);
        }
        decoded.as_slice()
    } else {
        path
    };
    let path = path.strip_prefix(prefix).ok_or_else(|| anyhow::anyhow!("invalid path in Git diff"))?;
    Ok(Some(std::str::from_utf8(path)?.to_owned()))
}
