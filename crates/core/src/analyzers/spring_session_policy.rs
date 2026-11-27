use std::path::{Component, Path};

use autoreview_schema::{AgentFinding, LocationRange};

use super::artifact_policy::{read_snapshot_file, Rule};

pub(super) const SECURE: Rule = Rule {
    id: "spring-session-cookie-without-secure",
    category: "security",
    subcategory: "auth.session-cookie",
    tool: "autoreview-spring-session-policy",
    title: "Spring session cookie does not require HTTPS",
    reference: "https://docs.spring.io/spring-boot/appendix/application-properties/index.html#application-properties.server.server.servlet.session.cookie.secure",
    enabled_by: "detected Spring Boot properties path",
};
pub(super) const HTTP_ONLY: Rule = Rule {
    id: "spring-session-cookie-without-http-only",
    title: "Browser scripts can read the Spring session cookie",
    reference: "https://docs.spring.io/spring-boot/appendix/application-properties/index.html#application-properties.server.server.servlet.session.cookie.http-only",
    ..SECURE
};

pub fn run_spring_session_policy(root: &Path, files: &[String]) -> Vec<AgentFinding> {
    let Ok(root) = root.canonicalize() else {
        return Vec::new();
    };
    files
        .iter()
        .filter(|path| is_application_properties(Path::new(path)))
        .flat_map(|path| {
            read_snapshot_file(&root, Path::new(path))
                .map_or_else(Vec::new, |source| check_properties(path, &source))
        })
        .collect()
}

fn is_application_properties(path: &Path) -> bool {
    let Some(stem) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".properties"))
    else {
        return false;
    };
    if stem != "application" && stem.strip_prefix("application-").is_none_or(str::is_empty) {
        return false;
    }
    let mut parent = path.components().rev().skip(1).peekable();
    if parent.peek() == Some(&Component::Normal("config".as_ref())) {
        parent.next();
    }
    ["resources", "main", "src"]
        .iter()
        .all(|part| parent.next() == Some(Component::Normal(part.as_ref())))
}

fn check_properties(path: &str, source: &str) -> Vec<AgentFinding> {
    let Some([secure, http_only]) = disabled_flags(source) else {
        return Vec::new();
    };
    [
        (
            &SECURE,
            secure.disabled_at(),
            "CWE-614",
            "Use Secure for session cookies in HTTPS deployments.",
        ),
        (
            &HTTP_ONLY,
            http_only.disabled_at(),
            "CWE-1004",
            "Keep HttpOnly enabled to prevent scripts from directly reading the session cookie.",
        ),
    ]
    .into_iter()
    .filter_map(|(rule, site, cwe, message)| {
        let (line, text) = site?;
        let range = LocationRange {
            start_line: line as u32 + 1,
            ..Default::default()
        };
        let mut finding = rule.finding(path, range, text, message.into());
        finding
            .meta
            .get_or_insert_with(Default::default)
            .insert("cwe".into(), serde_json::json!([cwe]));
        Some(finding)
    })
    .collect()
}

#[derive(Default)]
struct PropertySetting<'s> {
    spelling: Option<&'s str>,
    conflicting_spellings: bool,
    disabled: Option<(usize, &'s str)>,
}

impl<'s> PropertySetting<'s> {
    fn record(&mut self, key: &'s str, value: &str, site: (usize, &'s str)) {
        self.conflicting_spellings |= self.spelling.is_some_and(|previous| previous != key);
        self.spelling = Some(key);
        self.disabled = value.eq_ignore_ascii_case("false").then_some(site);
    }

    fn disabled_at(self) -> Option<(usize, &'s str)> {
        if self.conflicting_spellings {
            None
        } else {
            self.disabled
        }
    }
}

fn disabled_flags(source: &str) -> Option<[PropertySetting<'_>; 2]> {
    let mut settings = [PropertySetting::default(), PropertySetting::default()];
    for (line, raw) in source.lines().enumerate() {
        let text = raw.trim_start_matches(property_space);
        if matches!(text, "#---" | "!---") {
            return None;
        }
        if text.is_empty() || text.starts_with(['#', '!']) {
            continue;
        }
        let (key, value) = property(text);
        // A continuation can consume a later line. An escaped key can replace
        // a setting recorded under its ordinary spelling.
        if text.ends_with('\\') || key.contains('\\') {
            return None;
        }
        if let Some(index) = flag_index(key) {
            settings[index].record(key, value, (line, text));
        }
    }
    Some(settings)
}

fn flag_index(key: &str) -> Option<usize> {
    const PREFIX: &str = "server.servlet.session.cookie.";
    let prefix = key.get(..PREFIX.len())?;
    if !prefix.eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let flag = &key[PREFIX.len()..];
    if flag.eq_ignore_ascii_case("secure") {
        Some(0)
    } else if ["http-only", "http_only", "httpOnly"]
        .iter()
        .any(|name| flag.eq_ignore_ascii_case(name))
    {
        Some(1)
    } else {
        None
    }
}

fn property(line: &str) -> (&str, &str) {
    let split = line
        .find(|ch| matches!(ch, '=' | ':') || property_space(ch))
        .unwrap_or(line.len());
    let (key, value) = line.split_at(split);
    let value = value.trim_start_matches(property_space);
    let value = value.strip_prefix(['=', ':']).unwrap_or(value);
    (key, value.trim_matches(property_space))
}

fn property_space(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\u{c}')
}

#[cfg(test)]
mod tests {
    use super::{check_properties, is_application_properties};
    use std::path::Path;

    #[test]
    fn matches_main_resource_properties_and_final_explicit_flags() {
        for (path, supported) in [
            ("app/src/main/resources/application.properties", true),
            (
                "src/main/resources/config/application-preview.properties",
                true,
            ),
            ("src/test/resources/application.properties", false),
            ("docs/application.properties", false),
            ("src/main/resources/application-.properties", false),
            ("src/main/resources/application.yaml", false),
        ] {
            assert_eq!(
                is_application_properties(Path::new(path)),
                supported,
                "{path}"
            );
        }
        let source = r#"
# server.servlet.session.cookie.secure=false
server.servlet.session.cookie.secure=false
server.servlet.session.cookie.secure=true
server.servlet.session.cookie.http-only : false
unrelated.cookie.secure=false
server.servlet.session.cookie.secure false
"#;
        let findings = check_properties("application.properties", source);
        assert_eq!(findings.len(), 2);
        for (finding, (rule, line)) in findings.iter().zip([
            ("spring-session-cookie-without-secure", 7),
            ("spring-session-cookie-without-http-only", 5),
        ]) {
            assert_eq!(finding.source.rule_id.as_deref(), Some(rule));
            assert_eq!(finding.location.range.start_line, line);
        }
        assert!(check_properties("app", "server.servlet.session.cookie.http-only=false\nserver.servlet.session.cookie.httpOnly=${SESSION_HTTP_ONLY}").is_empty());
        assert!(check_properties("app", "server.servlet.session.cookie.http-only=false\nserver.servlet.session.cookie.http_only=true").is_empty());
        let conflicting = "server.servlet.session.cookie.http-only=true\nserver.servlet.session.cookie.httpOnly=false\nserver.servlet.session.cookie.secure=false";
        let findings = check_properties("app", conflicting);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].source.rule_id.as_deref(),
            Some("spring-session-cookie-without-secure")
        );
        assert!(check_properties("app", "server.servlet.session.cookie.secure=false\n#---\nserver.servlet.session.cookie.secure=true").is_empty());
        assert!(check_properties("app", "server.servlet.session.cookie.secure=false\nserver.servlet.session.cookie.sec\\ure=true").is_empty());
    }
}
