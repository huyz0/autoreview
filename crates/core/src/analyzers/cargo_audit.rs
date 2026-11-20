use std::path::Path;
use std::process::Command;

use anyhow::{ensure, Context};
use autoreview_schema::{
    AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct AuditReport {
    database: Database,
    vulnerabilities: Vulnerabilities,
}

#[derive(Deserialize)]
struct Database {
    #[serde(rename = "advisory-count")]
    advisory_count: usize,
}

#[derive(Deserialize)]
struct Vulnerabilities {
    found: bool,
    count: usize,
    list: Vec<Vulnerability>,
}

#[derive(Deserialize)]
struct Vulnerability {
    advisory: Advisory,
    package: Package,
    versions: Versions,
}

#[derive(Deserialize)]
struct Advisory {
    id: String,
    title: String,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
}

#[derive(Deserialize)]
struct Versions {
    patched: Vec<String>,
}

struct PackageLocation<'a> {
    name: &'a str,
    version: &'a str,
    source: Option<&'a str>,
    start_line: u32,
    end_line: u32,
    snippet: &'a str,
}

// Locate the fields written by Cargo. Audit validates the lockfile itself.
fn package_locations(lockfile: &str) -> Vec<PackageLocation<'_>> {
    let mut locations = Vec::new();
    let mut lines = lockfile.lines().enumerate().peekable();
    while let Some((_, line)) = lines.next() {
        if line.split('#').next().unwrap_or_default().trim() != "[[package]]" {
            continue;
        }
        let mut name = None;
        let mut version = None;
        let mut source = None;
        while let Some((index, line)) =
            lines.next_if(|(_, line)| !line.trim_start().starts_with('['))
        {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim_start();
            let Some(quote @ ('"' | '\'')) = value.chars().next() else {
                continue;
            };
            let Some((value, _)) = value[1..].split_once(quote) else {
                continue;
            };
            match key.trim() {
                "name" => name = Some((value, index as u32 + 1)),
                "version" => version = Some((value, index as u32 + 1, line.trim())),
                "source" => source = Some(value),
                _ => {}
            }
        }
        if let (Some((name, name_line)), Some((version, version_line, snippet))) = (name, version) {
            locations.push(PackageLocation {
                name,
                version,
                source,
                start_line: name_line.min(version_line),
                end_line: name_line.max(version_line),
                snippet,
            });
        }
    }
    locations
}

fn findings_from_report(
    path: &str,
    lockfile: &str,
    stdout: &[u8],
    exit_code: i32,
) -> anyhow::Result<Vec<AgentFinding>> {
    let report: AuditReport = serde_json::from_slice(stdout).context("invalid cargo-audit JSON")?;
    ensure!(
        report.database.advisory_count > 0,
        "cargo-audit's local advisory database is empty"
    );
    let vulnerabilities = report.vulnerabilities;
    ensure!(
        vulnerabilities.count == vulnerabilities.list.len()
            && vulnerabilities.found == (vulnerabilities.count > 0),
        "cargo-audit reported inconsistent vulnerability totals"
    );
    ensure!(
        (exit_code == 0 && !vulnerabilities.found) || (exit_code == 1 && vulnerabilities.found),
        "cargo-audit exited with status {exit_code} and no matching vulnerability report"
    );

    let locations = package_locations(lockfile);
    vulnerabilities.list.into_iter().map(|entry| {
        let Package { name, version, source } = entry.package;
        let mut matches = locations.iter().filter(|location| {
            location.name == name && location.version == version
                && source.as_deref().is_none_or(|source| location.source == Some(source))
        });
        let location = matches.next().with_context(|| {
            format!("could not locate audited package {name} {version} in {path}")
        })?;
        ensure!(matches.next().is_none(), "ambiguous audited package {name} {version} in {path}");
        let patched = if entry.versions.patched.is_empty() {
            "The advisory lists no patched version.".to_owned()
        } else {
            format!("Patched version requirements: {}.", entry.versions.patched.join(", "))
        };
        let advisory_id = entry.advisory.id;
        Ok(AgentFinding {
            source: FindingSource {
                kind: FindingSourceKind::Analyzer, tool: "cargo-audit".to_owned(),
                rule_id: Some(advisory_id.clone()), aspect: None, backend: None,
            },
            category: "security".to_owned(),
            severity: Severity::Medium,
            confidence: 1.0,
            title: format!("{advisory_id}: {}", entry.advisory.title),
            message: format!("{name} {version} is affected. {patched} https://rustsec.org/advisories/{advisory_id}.html"),
            location: Location {
                path: path.to_owned(),
                range: LocationRange { start_line: location.start_line, end_line: Some(location.end_line), ..Default::default() },
                snippet: location.snippet.to_owned(),
                side: Side::New,
            },
            related_locations: None,
            suggestion: None,
            tags: None,
            meta: None,
            suggested_patch: None,
        })
    }).collect()
}

/// Audits one lockfile from the committed review snapshot using the local
/// RustSec database. Running outside the snapshot prevents the reviewed
/// revision from changing the audit policy through `.cargo/audit.toml`.
pub fn run_cargo_audit(repo_root: &Path, lockfile: &str) -> anyhow::Result<Vec<AgentFinding>> {
    let lock_path = repo_root.join(lockfile);
    let metadata = match lock_path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let resolved_path = lock_path.canonicalize()?;
    ensure!(
        metadata.is_file() && resolved_path.starts_with(repo_root.canonicalize()?),
        "expected a regular lockfile inside the review snapshot"
    );
    let contents = std::fs::read_to_string(&resolved_path)?;
    let workdir = tempfile::tempdir()?;
    let output = Command::new("cargo")
        .current_dir(workdir.path())
        .args(["audit", "--json", "--no-fetch", "--no-yanked", "--file"])
        .arg(resolved_path)
        .output()
        .context("could not start cargo audit")?;
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    let details: Vec<_> = diagnostics.lines().take(5).collect();
    let status = output
        .status
        .code()
        .context("cargo audit terminated before completing")?;
    findings_from_report(lockfile, &contents, &output.stdout, status).with_context(|| {
        format!(
            "cargo audit did not yield a usable report ({})",
            details.join(" ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_vulnerabilities_to_the_matching_locked_package() {
        let lock = r#"
[[package]]
name = "db"
version = "1.0.0"
source = "git+https://example.invalid/db"
[[package]]
name = "db"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;
        let report = br#"{
          "database": {"advisory-count": 10, "last-updated": null},
          "vulnerabilities": {"found": true, "count": 1, "list": [
            {"advisory": {"id": "RUSTSEC-2026-0000", "title": "Example advisory"},
             "package": {"name": "db", "version": "1.0.0", "source": "registry+https://github.com/rust-lang/crates.io-index"},
             "versions": {"patched": [">=1.0.1"]}}
          ]}
        }"#;
        let findings = findings_from_report("app/Cargo.lock", lock, report, 1).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].source.rule_id.as_deref(),
            Some("RUSTSEC-2026-0000")
        );
        assert_eq!(findings[0].location.path, "app/Cargo.lock");
        assert_eq!(findings[0].location.range.start_line, 7);
        assert_eq!(findings[0].location.range.end_line, Some(8));
        assert!(findings[0].message.contains(">=1.0.1"));
    }

    #[test]
    fn distinguishes_a_clean_audit_from_process_and_report_failures() {
        let clean = br#"{"database":{"advisory-count":10},"vulnerabilities":{"found":false,"count":0,"list":[]}}"#;
        assert!(findings_from_report("Cargo.lock", "", clean, 0)
            .unwrap()
            .is_empty());
        assert!(findings_from_report("Cargo.lock", "", clean, 1).is_err());
        assert!(findings_from_report("Cargo.lock", "", b"not JSON", 0).is_err());
        assert!(findings_from_report("Cargo.lock", "", br#"{"database":{"advisory-count":10},"vulnerabilities":{"found":true,"count":1,"list":[]}}"#, 1).is_err());
    }
}
