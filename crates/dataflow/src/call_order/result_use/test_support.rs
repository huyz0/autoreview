use super::super::{ContractMode, KeyedFunction, ResultContract};
use super::{CallOrderSpec, ContractLanguage, KeyedUnit, Program};
use autoreview_langsupport::Language;

pub(super) fn with_program(
    languages: (Language, ContractLanguage),
    source: &str,
    api: (&str, &str),
    check: impl FnOnce(&Program<'_, '_, '_>, &[KeyedUnit<'_, '_, '_>]),
) {
    let (language, contract_language) = languages;
    let mut parser = autoreview_langsupport::parser_for(language).unwrap();
    let tree = parser.parse(source, None).unwrap();
    let root = tree.root_node();
    let nodes: Vec<_> = root
        .named_children(&mut root.walk())
        .filter(|node| matches!(node.kind(), "function_item" | "function_declaration"))
        .collect();
    let graphs: Vec<_> = nodes
        .iter()
        .map(|node| crate::lower::keyed_variant(language, source.as_bytes(), *node))
        .collect();
    let functions: Vec<_> = nodes
        .iter()
        .zip(&graphs)
        .map(|(node, graph)| KeyedFunction { node: *node, graph })
        .collect();
    let units = [KeyedUnit {
        path: "main",
        source: source.as_bytes(),
        functions: &functions,
        imports: &[],
        exports: &[],
        entry: true,
    }];
    let spec = CallOrderSpec {
        rule_id: "test".into(),
        after: vec![],
        unless: vec![],
        before: vec![],
        check_before_return: false,
        identity: None,
        contract: Some(ResultContract {
            mode: ContractMode::MustUse,
            language: contract_language,
            import: api.0.into(),
            methods: vec![api.1.into()],
            borrows: vec![],
            acquire_awaited: false,
            complete_observed: false,
            typed_promise_factory: false, typed_promise: false, typed_go_error: false,
        }),
    };
    check(&Program::new(&spec, &units), &units);
}
