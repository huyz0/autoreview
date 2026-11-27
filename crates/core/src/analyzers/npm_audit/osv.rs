use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{bail, ensure, Context};
use semver::Version;
use serde::Deserialize;

#[derive(Deserialize)]
struct Record {
    id: String,
    modified: String,
    withdrawn: Option<String>,
    #[serde(default)]
    affected: Vec<Affected>,
}

#[derive(Deserialize)]
struct Affected {
    package: Option<Package>,
    #[serde(default)]
    versions: Vec<String>,
    #[serde(default)]
    ranges: Vec<Range>,
}

#[derive(Deserialize)]
struct Package {
    ecosystem: String,
    name: String,
}

#[derive(Deserialize)]
struct Range {
    #[serde(rename = "type")]
    kind: String,
    events: Vec<BTreeMap<String, String>>,
}

enum Event {
    Introduced(Option<Version>),
    Fixed(Version),
    LastAffected(Version),
    Limit(Option<Version>),
}

impl Event {
    fn parse(fields: BTreeMap<String, String>) -> anyhow::Result<Self> {
        ensure!(fields.len() == 1, "expected exactly one OSV range event");
        let (kind, version) = fields.into_iter().next().context("empty OSV range event")?;
        Ok(match (kind.as_str(), version.as_str()) {
            ("introduced", "0") => Self::Introduced(None),
            ("introduced", version) => Self::Introduced(Some(Version::parse(version)?)),
            ("fixed", version) => Self::Fixed(Version::parse(version)?),
            ("last_affected", version) => Self::LastAffected(Version::parse(version)?),
            ("limit", "*") => Self::Limit(None),
            ("limit", version) => Self::Limit(Some(Version::parse(version)?)),
            _ => bail!("unknown OSV range event {kind}"),
        })
    }

    fn boundary(&self) -> Option<&Version> {
        match self {
            Self::Introduced(version) | Self::Limit(version) => version.as_ref(),
            Self::Fixed(version) | Self::LastAffected(version) => Some(version),
        }
    }
}

struct Timeline(Vec<Event>);

impl Timeline {
    fn parse(range: Range) -> anyhow::Result<Self> {
        let mut events = range
            .events
            .into_iter()
            .map(Event::parse)
            .collect::<anyhow::Result<Vec<_>>>()?;
        ensure!(
            events
                .iter()
                .any(|event| matches!(event, Event::Introduced(_))),
            "OSV range has no introduced event"
        );
        ensure!(
            !(events.iter().any(|event| matches!(event, Event::Fixed(_)))
                && events
                    .iter()
                    .any(|event| matches!(event, Event::LastAffected(_)))),
            "OSV range mixes fixed and last_affected events"
        );
        events.sort_by(|left, right| match (left.boundary(), right.boundary()) {
            (Some(left), Some(right)) => left.cmp_precedence(right),
            (left, right) => left.is_some().cmp(&right.is_some()),
        });
        Ok(Self(events))
    }

    fn contains(&self, version: &Version) -> bool {
        let mut limits = self
            .0
            .iter()
            .filter_map(|event| match event {
                Event::Limit(limit) => Some(limit.as_ref()),
                _ => None,
            })
            .peekable();
        if limits.peek().is_some()
            && !limits.any(|limit| limit.is_none_or(|limit| version.cmp_precedence(limit).is_lt()))
        {
            return false;
        }
        let mut affected = false;
        for event in &self.0 {
            match event {
                Event::Introduced(start)
                    if start
                        .as_ref()
                        .is_none_or(|start| !version.cmp_precedence(start).is_lt()) =>
                {
                    affected = true;
                }
                Event::Fixed(end) if !version.cmp_precedence(end).is_lt() => affected = false,
                Event::LastAffected(end) if version.cmp_precedence(end).is_gt() => affected = false,
                _ => {}
            }
        }
        affected
    }
}

struct Advisory {
    id: String,
    versions: Vec<Version>,
    timelines: Vec<Timeline>,
}

pub struct NpmAdvisories(BTreeMap<String, Vec<Advisory>>);

impl NpmAdvisories {
    pub fn load(directory: &Path) -> anyhow::Result<Self> {
        let mut files = Vec::new();
        for entry in
            std::fs::read_dir(directory).context("could not open the local OSV snapshot")?
        {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
            {
                files.push(entry.path());
            }
        }
        files.sort();
        ensure!(
            !files.is_empty(),
            "the local OSV snapshot contains no JSON records"
        );
        let mut advisories = BTreeMap::new();
        let mut seen = HashSet::new();
        for file in files {
            let record: Record = serde_json::from_reader(std::fs::File::open(&file)?)
                .with_context(|| format!("invalid OSV record {}", file.display()))?;
            ensure!(
                !record.id.is_empty()
                    && !record.modified.is_empty()
                    && record
                        .id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
                "invalid OSV identity in {}",
                file.display()
            );
            ensure!(
                seen.insert(record.id.clone()),
                "duplicate OSV record {}",
                record.id
            );
            if record.withdrawn.is_some() {
                continue;
            }
            add_record(&mut advisories, record)
                .with_context(|| format!("invalid npm OSV data in {}", file.display()))?;
        }
        Ok(Self(advisories))
    }

    pub(super) fn affected_ids<'a>(
        &'a self,
        name: &str,
        version: &'a Version,
    ) -> impl Iterator<Item = &'a str> {
        self.0
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(move |advisory| {
                (advisory
                    .versions
                    .iter()
                    .any(|known| known.cmp_precedence(version).is_eq())
                    || advisory
                        .timelines
                        .iter()
                        .any(|timeline| timeline.contains(version)))
                .then_some(advisory.id.as_str())
            })
    }
}

fn add_record(index: &mut BTreeMap<String, Vec<Advisory>>, record: Record) -> anyhow::Result<()> {
    for affected in record.affected {
        let Some(package) = affected
            .package
            .filter(|package| package.ecosystem == "npm")
        else {
            continue;
        };
        ensure!(!package.name.is_empty(), "empty npm package name");
        let mut timelines = Vec::new();
        for range in affected.ranges {
            match range.kind.as_str() {
                "SEMVER" | "ECOSYSTEM" => timelines.push(Timeline::parse(range)?),
                "GIT" => continue,
                kind => bail!("unsupported OSV range type {kind}"),
            }
        }
        let versions = affected
            .versions
            .iter()
            .map(|version| Version::parse(version))
            .collect::<Result<Vec<_>, _>>()?;
        index.entry(package.name).or_default().push(Advisory {
            id: record.id.clone(),
            versions,
            timelines,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline(events: &str) -> Timeline {
        Timeline::parse(Range {
            kind: "SEMVER".into(),
            events: serde_json::from_str(events).unwrap(),
        })
        .unwrap()
    }

    #[test]
    fn sorts_unsorted_events_and_preserves_fixed_and_introduced_boundaries() {
        let timeline = timeline(
            r#"[
            {"fixed":"3.0.1"},
            {"introduced":"3.0.0-rc.1"},
            {"fixed":"1.0.2"},
            {"introduced":"0"}
        ]"#,
        );
        for (version, affected) in [
            ("0.1.0", true),
            ("1.0.2", false),
            ("3.0.0-rc.0", false),
            ("3.0.0-rc.1+package.1", true),
            ("3.0.0", true),
            ("3.0.1", false),
        ] {
            assert_eq!(
                timeline.contains(&Version::parse(version).unwrap()),
                affected,
                "{version}"
            );
        }
    }

    #[test]
    fn last_affected_is_inclusive_and_limit_is_exclusive() {
        let last = timeline(r#"[{"introduced":"0"},{"last_affected":"2.0.0"}]"#);
        let limited = timeline(r#"[{"introduced":"0"},{"limit":"2.0.0"}]"#);
        let boundary = Version::parse("2.0.0").unwrap();
        assert!(last.contains(&boundary));
        assert!(!limited.contains(&boundary));
        assert!(!last.contains(&Version::parse("2.0.1").unwrap()));
        assert!(limited.contains(&Version::parse("1.9.9").unwrap()));
    }
}
