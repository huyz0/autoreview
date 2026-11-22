use autoreview_core::run_framework_checks;
use autoreview_test_support::init_repo;

#[test]
fn respects_expression_results_and_shadowed_string_bindings() {
    let samples = [
        (
            "Branch.kt",
            r#"fun render(name: String, flag: Boolean): String {
    val used = if (flag) { name.trim() } else { name.replace("a", "b") }
    if (flag) (name.trim())
    view { name.trim(); name.trim() }
    return used
}"#,
        ),
        (
            "scope.js",
            r#"const name = " a ";
function custom(name) { name.trim(); }
const other = name => { name.trim(); };
let dynamic = " a ";
dynamic = another();
dynamic.trim();
name.trim();
name.toUpperCase();
"#,
        ),
        (
            "owned.rs",
            r#"fn check() {
    let name = String::from(" a ");
    name.trim();
    let other = name.to_owned();
    other.trim();
}"#,
        ),
    ];
    for (path, source) in samples {
        let repo = init_repo(&[(path, source)]);
        let findings = run_framework_checks(repo.path(), &[path.into()]);
        assert_eq!(
            findings
                .iter()
                .filter(|finding| finding
                    .source
                    .rule_id
                    .as_deref()
                    .is_some_and(|id| id.ends_with("-discarded-string-transformation")))
                .count(),
            2,
            "{path}: {findings:#?}"
        );
    }
}

const SAMPLES: &[(&str, &str, &str)] = &[
    (
        "input.go",
        "go",
        r#"package sample
import transform "strings"
func Use(name string) string {
    transform.TrimSpace(name)
    transform.ReplaceAll(name, "a", "b")
    name = transform.ToLower(name)
    return transform.TrimSpace(name)
}
func Other(transform custom) { transform.TrimSpace("a") }
"#,
    ),
    (
        "Input.java",
        "java",
        r#"class Input {
    String convert(String name, Builder builder) {
        name.trim();
        name.replace("a", "b");
        builder.replace("a", "b");
        name = name.trim();
        return name.strip();
    }
}"#,
    ),
    (
        "Input.kt",
        "kotlin",
        r#"fun convert(name: String, builder: Builder): String {
    name.trim()
    val literal = " a "
    literal.replace("a", "b")
    builder.replace("a", "b")
    val kept = name.trim()
    return kept.replace("a", "b")
}
val mapper = { value: String -> value.trim() }
"#,
    ),
    (
        "input.js",
        "javascript",
        r#"function convert(obj) {
    let name = " a ";
    name.trim();
    name.replace("a", "b");
    name.replace(/a/, () => log());
    obj.trim();
    name = name.trim();
    return name.toLowerCase();
}"#,
    ),
    (
        "input.ts",
        "typescript",
        r#"function convert(name: string, obj: Builder) {
    name.trim();
    name.replace("a", "b");
    obj.trim();
    return name.trim().toLowerCase();
}"#,
    ),
    (
        "input.tsx",
        "typescript",
        r#"export function View(name: string) {
    name.trim();
    name.trim().replace("a", "b");
    return <p>{name.trim()}</p>;
}"#,
    ),
    (
        "input.rs",
        "rust",
        r#"fn convert(name: &str, mut owned: String) -> String {
    name.trim();
    owned.replace("a", "b");
    owned.make_ascii_lowercase();
    let _kept = name.trim();
    name.trim().to_uppercase()
}"#,
    ),
];

#[test]
fn detects_discarded_strings_across_supported_languages() {
    for &(path, language, source) in SAMPLES {
        let repo = init_repo(&[(path, source)]);
        let expected_id = format!("{language}-discarded-string-transformation");
        let findings = run_framework_checks(repo.path(), &[path.into()]);
        let strings: Vec<_> = findings
            .iter()
            .filter(|finding| finding.source.rule_id.as_deref() == Some(expected_id.as_str()))
            .collect();
        assert_eq!(strings.len(), 2, "{path}: {findings:#?}");
        for finding in strings {
            assert_eq!(finding.location.path, path);
            assert!(finding.location.snippet.ends_with(')'));
        }
    }
}
