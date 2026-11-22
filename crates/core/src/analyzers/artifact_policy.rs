use std::{ops::Range, path::Path};

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

use super::ast_grep::{RuleCatalogEntry, RuleDefinition};

pub(super) struct Rule {
    pub id: &'static str,
    pub category: &'static str,
    pub subcategory: &'static str,
    pub tool: &'static str,
    pub title: &'static str,
    pub reference: &'static str,
    pub enabled_by: &'static str,
}

impl Rule {
    pub fn entry(&self) -> RuleCatalogEntry {
        RuleCatalogEntry {
            id: self.id.into(), kind: "artifact".into(), category: self.category.into(),
            subcategories: vec![self.subcategory.into()],
        }
    }

    pub fn definition(&self) -> Option<RuleDefinition> {
        let entry = self.entry();
        let details = serde_json::json!({
            "id": self.id, "kind": entry.kind, "category": entry.category,
            "enabledBy": self.enabled_by,
            "metadata": self.metadata(),
        });
        Some(RuleDefinition {
            id: entry.id, source_label: "builtin".into(), kind: entry.kind,
            category: entry.category, semantic: false, yaml: serde_yaml::to_string(&details).ok()?,
        })
    }

    fn metadata(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::from([
            ("references".into(), serde_json::json!([self.reference])),
            ("subcategory".into(), serde_json::json!([self.subcategory])),
        ])
    }

    pub fn finding(&self, path: &str, range: LocationRange, snippet: &str, message: String) -> AgentFinding {
        AgentFinding {
            source: FindingSource {
                kind: FindingSourceKind::Analyzer, tool: self.tool.into(), rule_id: Some(self.id.into()),
                aspect: None, backend: None,
            },
            category: self.category.into(), severity: Severity::Medium, confidence: 1.0,
            title: self.title.into(), message,
            location: Location { path: path.into(), range, snippet: snippet.trim().into(), side: Side::New },
            related_locations: None, suggestion: None, tags: None, meta: Some(self.metadata()), suggested_patch: None,
        }
    }
}

pub(super) fn rules() -> impl Iterator<Item = &'static Rule> {
    [
        &super::workflow_policy::RULE,
        &super::postgres_migrations::RULE,
        &super::locale_catalogs::MISSING_PLACEHOLDER,
        &super::locale_catalogs::MISSING_OTHER,
        &super::protobuf_schema::REUSED_TAG,
        &super::protobuf_schema::UNRESERVED_TAG,
        &super::protobuf_schema::INCOMPATIBLE_FIELD,
    ].into_iter()
}

pub(super) fn read_snapshot_file(root: &Path, path: &Path) -> Option<String> {
    let requested = root.join(path);
    let resolved = requested.canonicalize().ok().filter(|file| file.starts_with(root))?;
    requested.symlink_metadata().is_ok_and(|meta| meta.is_file())
        .then(|| std::fs::read_to_string(resolved).ok()).flatten()
}

pub(super) fn source_lines(source: &str, bytes: &Range<usize>) -> LocationRange {
    let start_line = source[..bytes.start].bytes().filter(|byte| *byte == b'\n').count() as u32 + 1;
    let extra_lines = source[bytes.clone()].bytes().filter(|byte| *byte == b'\n').count() as u32;
    LocationRange { start_line, end_line: Some(start_line + extra_lines), ..Default::default() }
}
