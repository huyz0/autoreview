pub(super) fn subcategory_matches(label: &str, selector: &str) -> bool {
    let selector = selector.trim();
    label == selector || label.strip_prefix(selector).is_some_and(|suffix| suffix.starts_with('.'))
}

pub fn list_builtin(subcategories: &[String], json: bool) -> anyhow::Result<()> {
    let rules: Vec<_> = autoreview_core::list_builtin_rules().into_iter().filter(|rule| {
        subcategories.is_empty() || rule.subcategories.iter().any(|label| {
            subcategories.iter().any(|selector| subcategory_matches(label, selector))
        })
    }).collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&rules)?);
    } else {
        for rule in rules {
            println!("{}\t{}\t{}\t{}", rule.id, rule.category, rule.kind, rule.subcategories.join(", "));
        }
    }
    Ok(())
}
