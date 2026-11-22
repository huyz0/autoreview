mod cursor;
mod icu;
mod json;

use std::path::Path;

use autoreview_schema::{AgentFinding, LocaleCatalogConfig, Location, Side};

use super::artifact_policy::{read_snapshot_file, source_lines, Rule};

const REFERENCE: &str = "https://unicode-org.github.io/icu/userguide/format_parse/messages/";
const SUBCATEGORY: &str = "internationalization.message-contracts";
const TOOL: &str = "autoreview-locale-catalogs";
const ENABLED: &str = "artifactChecks.localeCatalogs";

pub(super) const MISSING_PLACEHOLDER: Rule = Rule {
    id: "icu-translation-drops-placeholder",
    category: "correctness",
    subcategory: SUBCATEGORY,
    tool: TOOL,
    title: "Translation omits an ICU placeholder",
    reference: REFERENCE,
    enabled_by: ENABLED,
};

pub(super) const MISSING_OTHER: Rule = Rule {
    id: "icu-translation-missing-other-branch",
    category: "correctness",
    subcategory: SUBCATEGORY,
    tool: TOOL,
    title: "ICU selector has no other branch",
    reference: REFERENCE,
    enabled_by: ENABLED,
};

pub fn run_locale_catalog_policy(root: &Path, changed: &[String], catalogs: &[LocaleCatalogConfig]) -> Vec<AgentFinding> {
    let Ok(root) = root.canonicalize() else { return Vec::new() };
    let mut findings = Vec::new();
    for config in catalogs {
        let Some(source) = read_snapshot_file(&root, Path::new(&config.reference)) else { continue };
        let Some(reference) = json::catalog(&source) else { continue };
        for target in &config.translations {
            if target == &config.reference || !(changed.contains(target) || changed.contains(&config.reference)) {
                continue;
            }
            let Some(translation) = read_snapshot_file(&root, Path::new(target)) else { continue };
            findings.extend(compare(target, &translation, &config.reference, &source, &reference));
        }
    }
    findings
}

fn compare(
    path: &str,
    content: &str,
    base_path: &str,
    base_content: &str,
    base: &std::collections::BTreeMap<Vec<String>, json::Message>,
) -> Vec<AgentFinding> {
    let Some(messages) = json::catalog(content) else { return Vec::new() };
    let mut findings = Vec::new();
    for (key, message) in messages {
        let Some(actual) = icu::signature(&message.decoded) else { continue };
        let label = key.join(".");
        let loc = source_lines(content, &message.range);
        if !actual.missing_other.is_empty() {
            let selectors = actual.missing_other.into_iter().collect::<Vec<_>>().join(", ");
            findings.push(MISSING_OTHER.finding(path, loc, &content[message.range.clone()],
                format!("Message {label} needs an other branch for {selectors}.")));
        }
        let Some(base_message) = base.get(&key) else { continue };
        let Some(expected) = icu::signature(&base_message.decoded) else { continue };
        let missing: Vec<_> = expected.placeholders.difference(&actual.placeholders).map(String::as_str).collect();
        if missing.is_empty() {
            continue;
        }
        let mut finding = MISSING_PLACEHOLDER.finding(path, loc, &content[message.range],
            format!("Message {label} omits {} from the reference locale.", missing.join(", ")));
        finding.related_locations = Some(vec![Location {
            path: base_path.into(),
            range: source_lines(base_content, &base_message.range),
            snippet: base_content[base_message.range.clone()].into(),
            side: Side::New,
        }]);
        findings.push(finding);
    }
    findings
}
