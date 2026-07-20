//! Parses `.autoreview/spec.md` into an `AcceptanceSpec` — the same
//! three-field shape Aviator's own spec format uses (`# Title`, `## Intent`,
//! `## Acceptance Criteria` bullets), hand-scanned line by line rather than
//! pulling in a markdown-parsing crate: this project's established
//! preference (see `extract_last_fenced_block`, `complexity.rs`'s
//! brace-scanner) for a format simple and fixed enough that a real parser
//! buys nothing.

use autoreview_schema::AcceptanceSpec;

#[derive(PartialEq)]
enum Section {
    None,
    Intent,
    Criteria,
}

fn heading_level(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = line[hashes..].trim();
    if rest.is_empty() {
        None
    } else {
        Some((hashes, rest))
    }
}

/// A bullet's marker (`-`/`*`) and optional GFM checkbox (`[ ]`/`[x]`) are
/// stripped — a criterion's text is what's checked, not whether the author
/// happened to pre-tick it.
fn parse_bullet(line: &str) -> Option<String> {
    let rest = line.strip_prefix("- ").or_else(|| line.strip_prefix("* "))?;
    let rest = rest.strip_prefix("[ ] ").or_else(|| rest.strip_prefix("[x] ")).or_else(|| rest.strip_prefix("[X] ")).unwrap_or(rest);
    let rest = rest.trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

/// Returns `None` when there's no usable spec — no `# Title` heading, or an
/// `## Acceptance Criteria` section with zero bullets — since a spec with
/// nothing to verify isn't meaningfully different from no spec at all,
/// mirroring `architecture.yaml`'s own "no file / no content = opt-out"
/// convention.
pub fn parse_spec(content: &str) -> Option<AcceptanceSpec> {
    let mut title: Option<String> = None;
    let mut intent_lines: Vec<String> = Vec::new();
    let mut criteria: Vec<String> = Vec::new();
    let mut section = Section::None;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if let Some((level, text)) = heading_level(line) {
            if level == 1 {
                if title.is_none() {
                    title = Some(text.to_string());
                }
                section = Section::None;
                continue;
            }
            section = match text.to_lowercase().as_str() {
                "intent" => Section::Intent,
                "acceptance criteria" => Section::Criteria,
                _ => Section::None,
            };
            continue;
        }
        match section {
            Section::Intent if !line.is_empty() => intent_lines.push(line.to_string()),
            Section::Criteria => {
                if let Some(item) = parse_bullet(line) {
                    criteria.push(item);
                }
            }
            _ => {}
        }
    }

    let title = title?;
    if criteria.is_empty() {
        return None;
    }
    Some(AcceptanceSpec { title, intent: intent_lines.join(" "), criteria })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_well_formed_spec() {
        let content = "\
# Add rate limiting

## Intent

Cap per-user request rate so a single client can't exhaust the API.

## Acceptance Criteria

- Returns 429 when a user exceeds the configured rate
- Includes a Retry-After header on the 429 response
- [ ] Existing endpoints are unaffected when under the limit
";
        let spec = parse_spec(content).expect("should parse");
        assert_eq!(spec.title, "Add rate limiting");
        assert_eq!(spec.intent, "Cap per-user request rate so a single client can't exhaust the API.");
        assert_eq!(spec.criteria, vec!["Returns 429 when a user exceeds the configured rate", "Includes a Retry-After header on the 429 response", "Existing endpoints are unaffected when under the limit"]);
    }

    #[test]
    fn accepts_asterisk_bullets_and_checked_checkboxes() {
        let content = "\
# T

## Acceptance Criteria

* [x] Done thing one
* Plain thing two
";
        let spec = parse_spec(content).unwrap();
        assert_eq!(spec.criteria, vec!["Done thing one", "Plain thing two"]);
    }

    #[test]
    fn no_title_heading_yields_no_spec() {
        assert!(parse_spec("## Acceptance Criteria\n\n- Something\n").is_none());
    }

    #[test]
    fn a_title_with_zero_criteria_yields_no_spec() {
        assert!(parse_spec("# Title only\n\n## Intent\n\nJust some prose.\n").is_none());
    }

    #[test]
    fn an_empty_criteria_section_yields_no_spec() {
        assert!(parse_spec("# T\n\n## Acceptance Criteria\n\n").is_none());
    }

    #[test]
    fn missing_intent_section_is_tolerated_as_an_empty_string() {
        let spec = parse_spec("# T\n\n## Acceptance Criteria\n\n- One\n").unwrap();
        assert_eq!(spec.intent, "");
    }

    #[test]
    fn text_outside_known_sections_is_ignored() {
        let content = "\
# T

Some preamble that isn't inside any recognized section.

## Some Other Heading

Unrelated prose that must not leak into intent or criteria.

## Acceptance Criteria

- Only this counts
";
        let spec = parse_spec(content).unwrap();
        assert_eq!(spec.criteria, vec!["Only this counts"]);
        assert_eq!(spec.intent, "");
    }
}
