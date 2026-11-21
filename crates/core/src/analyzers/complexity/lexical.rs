const CONTROL_KEYWORDS: &[&str] = &["if", "for", "while", "switch", "select", "catch", "else"];

/// Heuristic: does this line open a function/method body? Requires a
/// parenthesized parameter list followed by an opening brace on the same
/// line (the common "brace on same line as signature" style in all three
/// languages), and excludes lines that are actually control-flow statements
/// or type declarations that happen to also end in `) {` or `{`.
pub(crate) fn opens_function(trimmed: &str) -> Option<&str> {
    if !trimmed.ends_with('{') {
        return None;
    }
    let before_brace = trimmed[..trimmed.len() - 1].trim_end();
    if let Some(close_paren) = before_brace.rfind(')') {
        if let Some(open_paren) = before_brace[..close_paren].rfind('(') {
            let head = before_brace[..open_paren].trim();
            let first_word = head.split_whitespace().next().unwrap_or("");
            if !CONTROL_KEYWORDS.contains(&first_word) && !head.is_empty() {
                return Some(&before_brace[open_paren + 1..close_paren]);
            }
        }
    }
    opens_unparenthesized_arrow(before_brace)
}

/// JS/TS's single-parameter arrow function skips the parens entirely
/// (`x => {` / `async x => {`), which the paren-based check above can't
/// see at all — no `(`/`)` appears on the line. `=>` can't appear in this
/// position in Go/Java/Kotlin (Kotlin's own lambda arrow is `->`, a
/// different token), so this fallback is safe to run unconditionally
/// rather than gating it behind a language check.
fn opens_unparenthesized_arrow(before_brace: &str) -> Option<&str> {
    let before_arrow = before_brace.strip_suffix("=>")?.trim_end();
    // The param is whatever identifier-shaped token sits immediately
    // before `=>` — deliberately not requiring the *whole* line up to
    // this point to be just that identifier, since a real line is often
    // `items.map(x => {` (a chained call, not a bare assignment). `=>`
    // is unique to arrow-function syntax in JS/TS, so a trailing
    // `<ident> => {` is unambiguously an arrow function opening
    // regardless of what precedes it.
    let ident_start = before_arrow
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .map(|i| i + 1)
        .unwrap_or(0);
    let param = &before_arrow[ident_start..];
    let starts_like_identifier = param
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$');
    starts_like_identifier.then_some(param)
}

/// Extracts a method's name from its declaration line (the same shape
/// `opens_function` already validated) — the last identifier before the
/// parameter list, e.g. `"public String getName()"` -> `"getName"`. Used
/// for identifying named functions in line-based checks.
pub(crate) fn function_name(trimmed: &str) -> Option<&str> {
    if !trimmed.ends_with('{') {
        return None;
    }
    let before_brace = trimmed[..trimmed.len() - 1].trim_end();
    let close_paren = before_brace.rfind(')')?;
    let open_paren = before_brace[..close_paren].rfind('(')?;
    let head = before_brace[..open_paren].trim();
    head.split_whitespace().next_back()
}

pub(super) fn opens_class(trimmed: &str) -> bool {
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    trimmed.ends_with('{') && words.contains(&"class")
}

pub(super) fn opens_interface(trimmed: &str) -> bool {
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    trimmed.ends_with('{') && words.contains(&"interface")
}

/// Heuristic: does this line look like an abstract method signature inside
/// an interface body (no braces, just `name(...);`)? Distinct from
/// `opens_function`, which requires a same-line opening brace — interface
/// members are usually just a signature ending in `;`.
pub(super) fn is_abstract_member_signature(trimmed: &str) -> bool {
    if !trimmed.ends_with(';') || trimmed.starts_with("//") || trimmed.starts_with('@') {
        return false;
    }
    let Some(open_paren) = trimmed.find('(') else {
        return false;
    };
    trimmed[..open_paren]
        .trim()
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

pub(super) fn opens_control_block(trimmed: &str) -> bool {
    if !trimmed.ends_with('{') {
        return false;
    }
    let first_word = trimmed.split_whitespace().next().unwrap_or("");
    CONTROL_KEYWORDS.contains(&first_word) || trimmed.starts_with("} else")
}

/// Counts top-level (bracket/paren/angle-depth-0) commas in a parameter
/// list — so `foo(a []int, b map[string]int)` counts 2 params, not more,
/// despite the internal commas a naive split would trip on.
pub(super) fn count_params(param_list: &str) -> usize {
    let trimmed = param_list.trim();
    if trimmed.is_empty() {
        return 0;
    }
    let mut depth = 0i32;
    let mut count = 1usize;
    for c in trimmed.chars() {
        match c {
            '(' | '[' | '<' | '{' => depth += 1,
            ')' | ']' | '>' | '}' => depth -= 1,
            ',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    count
}
