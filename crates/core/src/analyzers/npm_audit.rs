use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{ensure, Context};
use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, Severity, Side};
use semver::Version;
use serde::Deserialize;
use serde_json::value::RawValue;

use super::artifact_policy::source_lines;

mod osv;
pub use osv::NpmAdvisories;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lockfile<'a> {
    lockfile_version: u32,
    #[serde(borrow)]
    packages: BTreeMap<String, &'a RawValue>,
}

#[derive(Deserialize)]
struct Package {
    name: Option<String>,
    version: Option<String>,
    resolved: Option<String>,
    #[serde(default)]
    link: bool,
}

fn registry_package<'a>(path: &'a str, package: &'a Package) -> Option<(&'a str, &'a str)> {
    let install_name = path
        .rsplit_once("/node_modules/")
        .map(|(_, name)| name)
        .or_else(|| path.strip_prefix("node_modules/"))?;
    let name = package.name.as_deref().unwrap_or(install_name);
    let version = package.version.as_deref()?;
    let resolved = package.resolved.as_deref()?;
    let basename = name.rsplit('/').next()?;
    let expected = format!("https://registry.npmjs.org/{name}/-/{basename}-{version}.tgz");
    (!package.link && resolved == expected).then_some((name, version))
}

fn findings(
    path: &str,
    source: &str,
    advisories: &NpmAdvisories,
) -> anyhow::Result<Vec<AgentFinding>> {
    let lockfile: Lockfile = serde_json::from_str(source).context("invalid package-lock.json")?;
    ensure!(
        matches!(lockfile.lockfile_version, 2 | 3),
        "expected npm lockfile version 2 or 3"
    );
    let mut findings = Vec::new();
    for (entry, raw) in lockfile.packages {
        let package: Package = serde_json::from_str(raw.get())
            .with_context(|| format!("invalid lockfile package {entry}"))?;
        let Some((name, raw_version)) = registry_package(&entry, &package) else {
            continue;
        };
        let version = Version::parse(raw_version)
            .with_context(|| format!("invalid locked version for {entry}"))?;
        let ids: BTreeSet<_> = advisories.affected_ids(name, &version).collect();
        if ids.is_empty() {
            continue;
        }
        // Borrowed raw JSON identifies the exact lockfile object even when the
        // same package is installed at multiple paths or formatted on one line.
        let start = raw.get().as_ptr() as usize - source.as_ptr() as usize;
        let range = source_lines(source, &(start..start + raw.get().len()));
        for id in ids {
            findings.push(AgentFinding {
                source: FindingSource {
                    kind: FindingSourceKind::Analyzer, tool: "autoreview-npm-osv".into(),
                    rule_id: Some(id.into()), aspect: None, backend: None,
                },
                category: "security".into(), severity: Severity::Medium, confidence: 1.0,
                title: format!("{id}: affected npm dependency"),
                message: format!("{name} {version} at {entry} is affected. Consult https://osv.dev/vulnerability/{id} for the affected ranges and upgrade guidance."),
                location: Location {
                    path: path.into(), range, snippet: format!("{entry} at {version}"),
                    side: Side::New,
                },
                related_locations: None, suggestion: None, tags: None, meta: None, suggested_patch: None,
            });
        }
    }
    Ok(findings)
}

/// Reads the committed lockfile from the review snapshot. The advisory index is
/// supplied by the reviewer and can be reused across lockfiles.
pub fn audit_npm_lock(
    root: &Path,
    lockfile: &str,
    advisories: &NpmAdvisories,
) -> anyhow::Result<Vec<AgentFinding>> {
    let path = root.join(lockfile);
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let resolved = path.canonicalize()?;
    ensure!(
        metadata.is_file() && resolved.starts_with(root.canonicalize()?),
        "expected a regular lockfile inside the review snapshot"
    );
    findings(lockfile, &std::fs::read_to_string(resolved)?, advisories)
}
