mod parser;

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;

use autoreview_schema::{AgentFinding, Location, Side};
use globset::{Glob, GlobSetBuilder};

use super::artifact_policy::{read_snapshot_file, source_lines, Rule};
use parser::{Field, Message};

const POLICY: &str = "artifactChecks.protobufCompatibility";
const SCHEMA_GUIDE: &str = "https://protobuf.dev/programming-guides/proto3/#updating";
const TOOL: &str = "autoreview-protobuf";

pub(super) const REUSED_TAG: Rule = Rule {
    id: "protobuf-field-tag-reused",
    category: "correctness",
    subcategory: "compatibility.schema-evolution",
    tool: TOOL,
    title: "Protobuf field number assigned to another field",
    reference: SCHEMA_GUIDE,
    enabled_by: POLICY,
};

pub(super) const UNRESERVED_TAG: Rule = Rule {
    id: "protobuf-removed-field-tag-not-reserved",
    category: "correctness",
    subcategory: "compatibility.schema-evolution",
    tool: TOOL,
    title: "Removed Protobuf field number is still available",
    reference: SCHEMA_GUIDE,
    enabled_by: POLICY,
};

pub(super) const INCOMPATIBLE_FIELD: Rule = Rule {
    id: "protobuf-incompatible-field-change",
    category: "correctness",
    subcategory: "compatibility.rolling-upgrades",
    tool: TOOL,
    title: "Protobuf field change can break mixed-version readers",
    reference: SCHEMA_GUIDE,
    enabled_by: POLICY,
};

struct Declaration<'a> {
    path: &'a str,
    source: &'a str,
    message: Message<'a>,
}

pub fn run_protobuf_policy(
    base: &Path,
    head: &Path,
    changed: &[String],
    globs: &[String],
) -> anyhow::Result<Vec<AgentFinding>> {
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        builder.add(Glob::new(glob)?);
    }
    let matches = builder.build()?;
    let paths: Vec<_> = changed
        .iter()
        .filter(|path| path.ends_with(".proto") && matches.is_match(path))
        .collect();
    let sources = |root: &Path| -> anyhow::Result<BTreeMap<String, String>> {
        let root = root.canonicalize()?;
        Ok(paths
            .iter()
            .filter_map(|path| {
                let content = read_snapshot_file(&root, Path::new(path))?;
                Some(((*path).clone(), content))
            })
            .collect())
    };
    Ok(compare_protobuf_schemas(&sources(base)?, &sources(head)?))
}

fn compare_protobuf_schemas(
    base: &BTreeMap<String, String>,
    head: &BTreeMap<String, String>,
) -> Vec<AgentFinding> {
    let before = declarations(base);
    let after = declarations(head);
    let mut findings = Vec::new();
    for (name, old) in before {
        let (Some(old), Some(Some(new))) = (old, after.get(&name)) else {
            continue;
        };
        compare_message(&name, &old, new, &mut findings);
    }
    findings
}

fn declarations(sources: &BTreeMap<String, String>) -> BTreeMap<String, Option<Declaration<'_>>> {
    let mut declarations = BTreeMap::new();
    for (path, source) in sources {
        let Some(messages) = parser::parse(source) else {
            continue;
        };
        for (name, message) in messages {
            declarations
                .entry(name)
                .and_modify(|existing| *existing = None)
                .or_insert(Some(Declaration {
                    path,
                    source,
                    message,
                }));
        }
    }
    declarations
}

fn compare_message(
    name: &str,
    old: &Declaration<'_>,
    new: &Declaration<'_>,
    findings: &mut Vec<AgentFinding>,
) {
    for (&tag, current) in &new.message.fields {
        let earlier = old.message.fields.get(&tag);
        if let Some(finding) = check_new_field(name, old, new, tag, earlier, current) {
            findings.push(finding);
        }
    }
    for (&tag, earlier) in &old.message.fields {
        if new.message.fields.contains_key(&tag) {
            continue;
        }
        if let Some(finding) = check_removed_field(name, old, new, tag, earlier) {
            findings.push(finding);
        }
    }
}

fn check_new_field(
    name: &str,
    old: &Declaration<'_>,
    new: &Declaration<'_>,
    tag: u32,
    earlier: Option<&Field<'_>>,
    current: &Field<'_>,
) -> Option<AgentFinding> {
    if earlier.is_none() {
        if let Some((&previous_tag, previous_field)) = old
            .message
            .fields
            .iter()
            .find(|(_, field)| field.name == current.name)
        {
            return Some(finding(
                &INCOMPATIBLE_FIELD,
                name,
                tag,
                format!("the field number changed from {previous_tag} to {tag}"),
                (new, &current.span, Side::New),
                (old, &previous_field.span, Side::Old),
            ));
        }
    }
    let (rule, reason) = match earlier {
        Some(field) => field_change(field, current, &old.message, &new.message)?,
        None if reserved(&old.message, tag) => (
            &REUSED_TAG,
            "the number was reserved in the base schema".into(),
        ),
        None if current.label == "required" => (
            &INCOMPATIBLE_FIELD,
            "messages from old writers do not contain this new required field".into(),
        ),
        None => return None,
    };
    let evidence = earlier.map_or(&old.message.span, |field| &field.span);
    Some(finding(
        rule,
        name,
        tag,
        reason,
        (new, &current.span, Side::New),
        (old, evidence, Side::Old),
    ))
}

fn check_removed_field(
    name: &str,
    old: &Declaration<'_>,
    new: &Declaration<'_>,
    tag: u32,
    earlier: &Field<'_>,
) -> Option<AgentFinding> {
    let (rule, reason) = if earlier.label == "required" {
        (
            &INCOMPATIBLE_FIELD,
            "old readers may reject messages that omit this required field",
        )
    } else if !reserved(&new.message, tag) {
        (
            &UNRESERVED_TAG,
            "reserve the deleted field number to prevent future reuse",
        )
    } else {
        return None;
    };
    Some(finding(
        rule,
        name,
        tag,
        reason.into(),
        (old, &earlier.span, Side::Old),
        (new, &new.message.span, Side::New),
    ))
}

fn reserved(message: &Message<'_>, tag: u32) -> bool {
    message
        .reserved
        .iter()
        .any(|&(start, end)| (start..=end).contains(&tag))
}

fn field_change(
    old: &Field<'_>,
    new: &Field<'_>,
    before: &Message<'_>,
    after: &Message<'_>,
) -> Option<(&'static Rule, String)> {
    if old.name != new.name {
        return Some((&REUSED_TAG, format!("the field name changed from `{}` to `{}`. Keep the original field number tied to its existing meaning", old.name, new.name)));
    }
    if type_changed(&old.kind, &new.kind) {
        return Some((
            &INCOMPATIBLE_FIELD,
            format!(
                "the field type changed from `{}` to `{}`",
                old.kind, new.kind
            ),
        ));
    }
    let label_changed = old.label != new.label
        && [old.label, new.label]
            .iter()
            .any(|label| matches!(*label, "required" | "repeated"));
    if label_changed {
        return Some((
            &INCOMPATIBLE_FIELD,
            "the field changed between repeated, required, or singular cardinality".into(),
        ));
    }
    if changes_oneof(old, new, before, after) {
        return Some((
            &INCOMPATIBLE_FIELD,
            "the field moved into or out of a oneof that can discard another value".into(),
        ));
    }
    None
}

fn type_changed(before: &str, after: &str) -> bool {
    if before == after {
        return false;
    }
    if let (Some(before), Some(after)) = (map_types(before), map_types(after)) {
        return type_changed(before.0, after.0) || type_changed(before.1, after.1);
    }
    // Qualifying the same local or imported message can leave its meaning unchanged.
    if !before.starts_with("map<")
        && !after.starts_with("map<")
        && before.rsplit('.').next() == after.rsplit('.').next()
    {
        return before.contains('.')
            && after.contains('.')
            && before.trim_start_matches('.') != after.trim_start_matches('.');
    }
    true
}

fn map_types(kind: &str) -> Option<(&str, &str)> {
    kind.strip_prefix("map<")?
        .strip_suffix('>')?
        .split_once(',')
}

fn changes_oneof(
    old: &Field<'_>,
    new: &Field<'_>,
    before: &Message<'_>,
    after: &Message<'_>,
) -> bool {
    if old.oneof == new.oneof {
        return false;
    }
    let shared = |group: Option<&str>, message: &Message<'_>| {
        group.is_some_and(|name| {
            message
                .fields
                .values()
                .filter(|field| field.oneof == Some(name))
                .nth(1)
                .is_some()
        })
    };
    shared(old.oneof, before) || shared(new.oneof, after)
}

type Evidence<'a> = (&'a Declaration<'a>, &'a Range<usize>, Side);

fn location((declaration, span, side): Evidence<'_>) -> Location {
    Location {
        path: declaration.path.to_owned(),
        range: source_lines(declaration.source, span),
        snippet: declaration.source[span.clone()].trim().to_owned(),
        side,
    }
}

fn finding(
    rule: &Rule,
    name: &str,
    tag: u32,
    reason: String,
    primary: Evidence<'_>,
    related: Evidence<'_>,
) -> AgentFinding {
    let primary = location(primary);
    let mut finding = rule.finding(
        &primary.path,
        primary.range,
        &primary.snippet,
        format!("`{name}` field {tag}: {reason}."),
    );
    finding.location.side = primary.side;
    finding.related_locations = Some(vec![location(related)]);
    finding
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_moved_messages_and_distinguishes_oneof_changes() {
        let base = BTreeMap::from([(
            "old.proto".into(),
            "
            package acme;
            message Payload {
                string a = 1;
                bytes b = 2;
                optional int32 c = 3;
                map<string, Value> d = 4;
            }
        "
            .into(),
        )]);
        let head = BTreeMap::from([(
            "new.proto".into(),
            "
            package acme;
            message Payload {
                oneof pair { string a = 1; bytes b = 2; }
                oneof single { int32 c = 3; }
                map<string, .acme.Value> d = 4;
                int32 e = 5;
            }
        "
            .into(),
        )]);
        let findings = compare_protobuf_schemas(&base, &head);
        assert_eq!(findings.len(), 2);
        for (tag, finding) in (1..=2).zip(findings) {
            assert!(finding.message.contains(&format!("field {tag}:")));
            assert_eq!(finding.location.path, "new.proto");
            assert_eq!(finding.related_locations.unwrap()[0].path, "old.proto");
        }
    }

    #[test]
    fn renumbering_a_field_breaks_compatibility_even_if_its_old_number_is_reserved() {
        let base = BTreeMap::from([(
            "schema.proto".into(),
            "message Foo { string value = 1; }".into(),
        )]);
        let head = BTreeMap::from([(
            "schema.proto".into(),
            "message Foo { reserved 1; string value = 2; }".into(),
        )]);
        let findings = compare_protobuf_schemas(&base, &head);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].source.rule_id.as_deref(),
            Some(INCOMPATIBLE_FIELD.id)
        );
        assert!(findings[0]
            .message
            .contains("field number changed from 1 to 2"));
    }
}
