use autoreview_schema::Severity;
use tree_sitter::Node;

use super::basics::call_path;
use super::syntax::{
    ancestors, arguments, children, is_comment, is_execution_boundary, method_call, Source,
};
use super::{Review, RustReviewRule};

const RAW_BUILDER_VALUE: RustReviewRule = RustReviewRule {
    id: "rust-sqlx-builder-interpolated-value",
    category: "security",
    severity: Severity::Medium,
    semantic: true,
    title: "Interpolated SQLx query builder text",
    message: "This SQLx query builder inserts a formatted value as SQL text. Use push_bind for data values and a closed allowlist for SQL identifiers.",
    reference: "https://docs.rs/sqlx/latest/sqlx/struct.QueryBuilder.html#method.push",
    cwe: Some("CWE-89"),
};
const RAW_QUERY: RustReviewRule = RustReviewRule {
    id: "rust-interpolated-raw-sql",
    category: "security",
    severity: Severity::Medium,
    semantic: true,
    title: "Interpolated raw SQL statement",
    message: "This query interpolates a value into SQL text. Bind dynamic data through query parameters and use a closed allowlist for identifiers.",
    reference: "https://docs.rs/sqlx/latest/sqlx/fn.query.html#dynamic-input-use-query-parameters-prevents-sql-injection",
    cwe: Some("CWE-89"),
};
const RUSQLITE_RAW: RustReviewRule = RustReviewRule {
    id: "rust-rusqlite-interpolated-value",
    category: "security",
    severity: Severity::Medium,
    semantic: true,
    title: "Interpolated rusqlite statement",
    message: "This rusqlite call inserts a formatted value into a SQL statement. Bind data values through statement parameters.",
    reference: "https://docs.rs/rusqlite/latest/rusqlite/struct.Connection.html#method.execute",
    cwe: Some("CWE-89"),
};

const N_PLUS_ONE: RustReviewRule = RustReviewRule {
    id: "rust-sqlx-query-per-row",
    category: "performance",
    severity: Severity::Medium,
    semantic: true,
    title: "SQLx query executed for each returned row",
    message: "This loop runs a second query with data from each row of the first query. Consider loading the related rows in one query or batching by key.",
    reference: "https://diesel.rs/guides/relations.html",
    cwe: None,
};
const POOL_WRITE: RustReviewRule = RustReviewRule {
    id: "rust-sqlx-pool-write-during-transaction",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "SQLx write uses the pool during a transaction",
    message: "This write uses the pool directly while a transaction from that pool is active. If the write should commit or roll back with the transaction, pass the transaction as the executor.",
    reference: "https://docs.rs/sqlx/latest/sqlx/struct.Transaction.html",
    cwe: None,
};

pub(super) const RULES: &[&RustReviewRule] = &[
    &RAW_BUILDER_VALUE,
    &RAW_QUERY,
    &RUSQLITE_RAW,
    &N_PLUS_ONE,
    &POOL_WRITE,
];

pub(super) fn check<'tree>(review: &mut Review<'tree, '_>, node: Node<'tree>) {
    if node.kind() == "call_expression" {
        if interpolates_raw_query(&review.source, node) {
            review.report(&RAW_QUERY, node);
        } else if uses_raw_builder_text(&review.source, node) {
            review.report(&RAW_BUILDER_VALUE, node);
        } else if interpolates_rusqlite_statement(&review.source, node) {
            review.report(&RUSQLITE_RAW, node);
        }
    }
    if node.kind() == "await_expression" {
        if let Some(origin) = per_row_query_origin(&review.source, node) {
            review.report_related(&N_PLUS_ONE, node, &[origin]);
        }
    }
    if node.kind() == "block" {
        for (write, begin, commit) in writes_outside_transaction(&review.source, node) {
            review.report_related(&POOL_WRITE, write, &[begin, commit]);
        }
    }
}

fn interpolates_raw_query<'tree>(source: &Source<'tree, '_>, node: Node<'tree>) -> bool {
    if !matches!(
        call_path(source, node).as_deref(),
        Some(
            "sqlx::query"
                | "sqlx::query_as"
                | "sqlx::query_scalar"
                | "sqlx::query_with"
                | "sqlx::query_as_with"
                | "sqlx::query_scalar_with"
                | "sqlx::raw_sql"
                | "diesel::sql_query"
        )
    ) {
        return false;
    }
    let Some(value) = arguments(node).next() else {
        return false;
    };
    let sql = unborrow(value);
    let sql = if call_path(source, sql).as_deref() == Some("sqlx::AssertSqlSafe") {
        let Some(value) = arguments(sql).next() else {
            return false;
        };
        value
    } else {
        sql
    };
    sql.child_by_field_name("macro")
        .is_some_and(|name| source.standard_macro(name, &["format"]))
        && dynamic_format(source, sql)
}

fn interpolates_rusqlite_statement<'tree>(source: &Source<'tree, '_>, node: Node<'tree>) -> bool {
    let Some((receiver, method)) = method_call(node) else {
        return false;
    };
    if !matches!(
        source.text(method),
        "execute" | "execute_batch" | "query_row" | "prepare" | "prepare_cached"
    ) || !arguments(node)
        .next()
        .is_some_and(|value| dynamic_format(source, unborrow(value)))
    {
        return false;
    }
    source
        .declaration(receiver)
        .and_then(|decl| declared_type(source, decl))
        .as_deref()
        == Some("rusqlite::Connection")
}

fn writes_outside_transaction<'tree>(
    source: &Source<'tree, '_>,
    block: Node<'tree>,
) -> Vec<(Node<'tree>, Node<'tree>, Node<'tree>)> {
    let statements: Vec<_> = children(block).filter(|part| !is_comment(*part)).collect();
    let mut findings = Vec::new();
    for (index, &binding) in statements.iter().enumerate() {
        if let Some(pool) = transaction_pool(source, binding) {
            findings.extend(pool_writes_before_commit(
                source,
                pool,
                binding,
                &statements[index + 1..],
            ));
        }
    }
    findings
}

fn transaction_pool<'tree>(
    source: &Source<'tree, '_>,
    binding: Node<'tree>,
) -> Option<Node<'tree>> {
    let init = binding.child_by_field_name("value")?;
    if binding.kind() != "let_declaration" {
        return None;
    }
    let (receiver, method) = method_call(awaited_call(init)?)?;
    if source.text(method) != "begin" {
        return None;
    }
    let pool = source.declaration(receiver)?;
    matches!(
        declared_type(source, pool).as_deref(),
        Some("sqlx::PgPool" | "sqlx::MySqlPool" | "sqlx::SqlitePool" | "sqlx::Pool")
    )
    .then_some(pool)
}

fn declared_type<'tree>(source: &Source<'tree, '_>, declaration: Node<'tree>) -> Option<String> {
    let mut ty = declaration.child_by_field_name("type")?;
    if ty.kind() == "reference_type" {
        ty = ty.child_by_field_name("type")?;
    }
    source.external_path(ty)
}

fn pool_writes_before_commit<'tree>(
    source: &Source<'tree, '_>,
    pool: Node<'tree>,
    transaction: Node<'tree>,
    after: &[Node<'tree>],
) -> Vec<(Node<'tree>, Node<'tree>, Node<'tree>)> {
    let mut writes = Vec::new();
    for &statement in after {
        let Some(call) = awaited_call(statement) else {
            break;
        };
        if let Some((receiver, operation)) = method_call(call) {
            if source.text(operation) == "commit"
                && source.local_binding(receiver) == Some(transaction)
            {
                return writes
                    .into_iter()
                    .map(|write| (write, transaction, statement))
                    .collect();
            }
        }
        // A contiguous sequence of known writes cannot move or rebind the
        // transaction. Stop when the code moves outside that proof.
        let Some(query) = query_receiver(source, call, &["execute"]) else {
            break;
        };
        if !has_simple_bind_values(query) {
            break;
        }
        if query_constructor(source, query)
            .and_then(|constructor| arguments(constructor).next())
            .is_some_and(|sql| is_write_sql(source, sql))
            && arguments(call)
                .next()
                .is_some_and(|executor| source.declaration(unborrow(executor)) == Some(pool))
        {
            writes.push(call);
        }
    }
    Vec::new()
}

fn has_simple_bind_values(mut query: Node<'_>) -> bool {
    while let Some((receiver, _)) = method_call(query) {
        if !arguments(query).all(simple_value) {
            return false;
        }
        query = receiver;
    }
    arguments(query).all(simple_value)
}

fn simple_value(value: Node<'_>) -> bool {
    match value.kind() {
        "identifier" | "string_literal" | "raw_string_literal" | "integer_literal"
        | "float_literal" | "boolean_literal" | "char_literal" => true,
        "field_expression" | "reference_expression" => {
            value.child_by_field_name("value").is_some_and(simple_value)
        }
        _ => false,
    }
}

fn is_write_sql(source: &Source<'_, '_>, sql: Node<'_>) -> bool {
    let Some(word) = children(sql)
        .find(|part| part.kind() == "string_content")
        .and_then(|content| source.text(content).split_ascii_whitespace().next())
    else {
        return false;
    };
    ["INSERT", "UPDATE", "DELETE", "REPLACE"]
        .into_iter()
        .any(|verb| word.eq_ignore_ascii_case(verb))
}

fn awaited_call(mut expression: Node<'_>) -> Option<Node<'_>> {
    if expression.kind() == "expression_statement" {
        expression = children(expression).find(|part| !is_comment(*part))?;
    }
    if expression.kind() == "try_expression" {
        expression = children(expression).find(|part| !is_comment(*part))?;
    }
    if expression.kind() != "await_expression" {
        return None;
    }
    children(expression).find(|part| part.kind() == "call_expression")
}

fn uses_raw_builder_text<'tree>(source: &Source<'tree, '_>, node: Node<'tree>) -> bool {
    if !arguments(node)
        .next()
        .is_some_and(|value| dynamic_format(source, value))
    {
        return false;
    }
    if is_builder_new(source, node) {
        return true;
    }
    let Some((receiver, method)) = method_call(node) else {
        return false;
    };
    if source.text(method) != "push" {
        return false;
    }
    if is_builder_new(source, receiver) {
        return true;
    }
    source.local_binding(receiver).is_some_and(|binding| {
        binding
            .child_by_field_name("value")
            .is_some_and(|value| is_builder_new(source, value))
    })
}

fn is_builder_new<'tree>(source: &Source<'tree, '_>, node: Node<'tree>) -> bool {
    call_path(source, node).as_deref() == Some("sqlx::QueryBuilder::new")
}

fn dynamic_format<'tree>(source: &Source<'tree, '_>, node: Node<'tree>) -> bool {
    if node.kind() != "macro_invocation" {
        return false;
    }
    let Some(name) = node.child_by_field_name("macro") else {
        return false;
    };
    if !source.standard_macro(name, &["format", "format_args"]) {
        return false;
    }
    let Some(tokens) = children(node).find(|node| node.kind() == "token_tree") else {
        return false;
    };
    let mut args = children(tokens).filter(|token| !is_comment(*token));
    let Some(template) = args
        .next()
        .filter(|token| matches!(token.kind(), "string_literal" | "raw_string_literal"))
    else {
        return false;
    };
    if children(template)
        .filter(|part| part.kind() == "escape_sequence")
        .any(|part| brace_escape(source.text(part)))
    {
        return false;
    }
    let args: Vec<_> = args.collect();
    let constant_arguments = !args.is_empty() && args.into_iter().all(simple_literal);
    let mut bytes = source.text(template).as_bytes().iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            // Rust string escapes have their own tree nodes. In particular
            // the braces in a Unicode escape do not interpolate a value.
            b'\\' if template.kind() == "string_literal" => {
                if bytes.next() == Some(b'u') && bytes.next() == Some(b'{') {
                    for next in bytes.by_ref() {
                        if next == b'}' {
                            break;
                        }
                    }
                }
            }
            b'{' if bytes.next_if_eq(&b'{').is_none()
                && (!constant_arguments
                    || bytes
                        .peek()
                        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn simple_literal(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "string_literal"
            | "raw_string_literal"
            | "integer_literal"
            | "float_literal"
            | "boolean_literal"
            | "char_literal"
    )
}

fn brace_escape(text: &str) -> bool {
    let code = text.strip_prefix("\\x").map(str::to_owned).or_else(|| {
        text.strip_prefix("\\u{")?
            .strip_suffix('}')
            .map(|code| code.replace('_', ""))
    });
    code.and_then(|code| u32::from_str_radix(&code, 16).ok())
        .is_some_and(|code| matches!(code, 0x7B | 0x7D))
}

fn per_row_query_origin<'tree>(
    source: &Source<'tree, '_>,
    awaited: Node<'tree>,
) -> Option<Node<'tree>> {
    let call = children(awaited).find(|node| !is_comment(*node))?;
    let query = query_receiver(source, call, &["fetch_all", "fetch_one", "fetch_optional"])?;
    let iteration = ancestors(awaited)
        .take_while(|node| !is_execution_boundary(*node))
        .find(|node| {
            matches!(
                node.kind(),
                "for_expression" | "while_expression" | "loop_expression"
            )
        })
        .filter(|node| node.kind() == "for_expression")?;
    let body = iteration.child_by_field_name("body")?;
    if awaited.start_byte() < body.start_byte() || !has_row_bound(source, query, iteration) {
        return None;
    }

    let rows = unborrow(iteration.child_by_field_name("value")?);
    let binding = source.local_binding(rows)?;
    if children(binding).any(|node| node.kind() == "mutable_specifier") {
        return None;
    }
    let origin = binding.child_by_field_name("value")?;
    let awaited = if origin.kind() == "try_expression" {
        children(origin).find(|node| !is_comment(*node))?
    } else {
        origin
    };
    let origin_query = children(awaited).find(|node| !is_comment(*node))?;
    (awaited.kind() == "await_expression"
        && query_receiver(source, origin_query, &["fetch_all"]).is_some())
    .then_some(binding)
}

fn unborrow(node: Node<'_>) -> Node<'_> {
    if node.kind() == "reference_expression" {
        node.child_by_field_name("value").unwrap_or(node)
    } else {
        node
    }
}

fn query_receiver<'tree>(
    source: &Source<'tree, '_>,
    terminal: Node<'tree>,
    operations: &[&str],
) -> Option<Node<'tree>> {
    let (query, operation) = method_call(terminal)?;
    if !operations.contains(&source.text(operation)) {
        return None;
    }
    query_constructor(source, query).map(|_| query)
}

fn query_constructor<'tree>(
    source: &Source<'tree, '_>,
    mut current: Node<'tree>,
) -> Option<Node<'tree>> {
    while let Some((earlier, name)) = method_call(current) {
        if !matches!(source.text(name), "bind" | "persistent") {
            return None;
        }
        current = earlier;
    }
    matches!(
        call_path(source, current).as_deref(),
        Some("sqlx::query" | "sqlx::query_as" | "sqlx::query_scalar")
    )
    .then_some(current)
}

fn has_row_bound<'tree>(
    source: &Source<'tree, '_>,
    mut query: Node<'tree>,
    iteration: Node<'tree>,
) -> bool {
    while let Some((previous, method)) = method_call(query) {
        if source.text(method) == "bind"
            && arguments(query).any(|argument| row_projection(source, argument, iteration))
        {
            return true;
        }
        query = previous;
    }
    false
}

fn row_projection<'tree>(
    source: &Source<'tree, '_>,
    mut value: Node<'tree>,
    iteration: Node<'tree>,
) -> bool {
    while matches!(value.kind(), "field_expression" | "reference_expression") {
        let Some(parent) = value.child_by_field_name("value") else {
            return false;
        };
        value = parent;
    }
    source.references_iteration_binding(value, iteration)
}

#[cfg(test)]
mod tests {
    use super::super::analyze_file;

    #[test]
    fn resolves_raw_queries_and_checks_the_actual_interpolation() {
        let findings = analyze_file(
            "lib.rs",
            r#"
use sqlx::{query_as as select, AssertSqlSafe};
fn statements(name: &str) {
    select::<_, Customer>(AssertSqlSafe(format!("SELECT * FROM customers WHERE name = '{name}'")));
    sqlx::query(&format!("SELECT id FROM customers WHERE name = '{}'", name));
    diesel::sql_query(format!("SELECT * FROM customers WHERE name = '{}'", name));
    sqlx::query("SELECT id FROM customers WHERE name = $1").bind(name);
    sqlx::query!("SELECT id FROM customers WHERE name = $1", name);
    diesel::sql_query(format!("SELECT '{}'", "fixed"));
    diesel::sql_query(format!("SELECT '{{}}'"));
}
"#,
        );
        assert_eq!(findings.len(), 3);
        assert!(findings
            .iter()
            .all(|finding| finding.source.rule_id.as_deref() == Some("rust-interpolated-raw-sql")));
    }

    #[test]
    fn finds_formatted_builder_text_but_keeps_bound_values_quiet() {
        let findings = analyze_file(
            "lib.rs",
            r#"
use sqlx::{QueryBuilder as Builder, Postgres};
fn find(value: &str) {
    let mut related = Builder::<Postgres>::new("SELECT id FROM item WHERE name = ");
    related.push(format!("'{value}'"));
    related.push(" ORDER BY id");
    related.push(format!(" LIMIT 1"));
    related.push(format!(" /* \u{2603} */"));
    related.push(format!(" /* {{}} */"));
    related.push(format!(" /* \u{7b}{ */"));
    related.push_bind(value);
    {
        let mut related = OtherBuilder::new();
        related.push(format!("{value}"));
    }
}
"#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].source.rule_id.as_deref(),
            Some("rust-sqlx-builder-interpolated-value")
        );
    }

    #[test]
    fn ignores_macros_that_shadow_the_standard_formatter() {
        let findings = analyze_file(
            "lib.rs",
            r#"
macro_rules! format { ($ignored:literal) => { "SELECT 1" }; }
mod nested {
    fn query() {
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT ");
        builder.push(format!("{fixed}"));
    }
}
"#,
        );
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[test]
    fn proves_the_parent_query_row_binding_and_awaited_child_query() {
        let findings = analyze_file(
            "lib.rs",
            r#"
use sqlx::query_as as query;
async fn lookup(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    let rows = query::<_, Parent>("SELECT id FROM parent").fetch_all(pool).await?;
    for row in &rows {
        query::<_, Child>("SELECT id FROM child WHERE parent = $1")
            .bind(row.id).fetch_all(pool).await?;
        query::<_, Child>("SELECT id FROM child").fetch_all(pool).await?;
        query::<_, Child>("SELECT id FROM child WHERE parent = $1")
            .bind({ let _ = row.id; 1 }).fetch_all(pool).await?;
        let _not_executed = query::<_, Child>("SELECT id FROM child WHERE parent = $1").bind(row.id);
        {
            let row = unrelated();
            query::<_, Child>("SELECT id FROM child WHERE parent = $1")
                .bind(row.id).fetch_all(pool).await?;
        }
    }
    Ok(())
}
"#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].source.rule_id.as_deref(),
            Some("rust-sqlx-query-per-row")
        );
        assert_eq!(
            findings[0].related_locations.as_ref().unwrap()[0]
                .range
                .start_line,
            4
        );
    }

    #[test]
    fn distinguishes_pool_writes_from_writes_in_the_transaction() {
        let findings = analyze_file(
            "lib.rs",
            r#"
use sqlx::{PgPool, query as write};
async fn update(pool: &PgPool, other: &PgPool) -> sqlx::Result<()> {
    let mut transaction = pool.begin().await?;
    write("UPDATE items SET active = true").execute(&mut *transaction).await?;
    write("UPDATE items SET active = true").execute(other).await?;
    write("UPDATE items SET active = false").execute(pool).await?;
    write("SELECT 1").execute(pool).await?;
    transaction.commit().await?;
    write("UPDATE items SET active = true").execute(pool).await?;
    Ok(())
}
async fn replaces_transaction(pool: &PgPool, other: &PgPool) -> sqlx::Result<()> {
    let mut transaction = pool.begin().await?;
    write("UPDATE items SET active = $1")
        .bind({ transaction = other.begin().await?; true }).execute(pool).await?;
    transaction.commit().await?;
    Ok(())
}
"#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].source.rule_id.as_deref(),
            Some("rust-sqlx-pool-write-during-transaction")
        );
        assert_eq!(findings[0].location.range.start_line, 7);
    }

    #[test]
    fn resolves_rusqlite_connection_types_before_reporting_raw_sql() {
        let findings = analyze_file(
            "lib.rs",
            r#"
use rusqlite::Connection as Db;
fn change(db: &Db, name: &str) {
    db.execute(&format!("UPDATE items SET name = '{}'", name), []);
    db.execute("UPDATE items SET name = ?1", [name]);
    {
        let db = mock_connection();
        db.execute(&format!("UPDATE items SET name = '{}'", name), []);
    }
}
"#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].source.rule_id.as_deref(),
            Some("rust-rusqlite-interpolated-value")
        );
    }
}
