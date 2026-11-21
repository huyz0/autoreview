const DEFAULT_MAX_METHOD_LINES: usize = 80;
const DEFAULT_MAX_PARAMS: usize = 5;
const DEFAULT_MAX_NESTING: usize = 4;
const DEFAULT_MAX_CLASS_MEMBERS: usize = 20;
const DEFAULT_MAX_RUST_MODULE_ITEMS: usize = 20;
/// Fowler's Switch Statements smell doesn't give a specific arm count, so
/// this follows the same style as the other thresholds here: conservative
/// enough that a legitimately large, unavoidable dispatch (a state machine,
/// a protocol opcode table) doesn't get flagged constantly, while still
/// catching the "this should probably be polymorphism/a lookup table"
/// cases the smell is about.
const DEFAULT_MAX_SWITCH_CASES: usize = 8;
/// A class needs at least this many trivial-accessor-shaped methods before
/// Data Class is even considered — a 1- or 2-method match (e.g. a real
/// class that happens to have one getter) is too common to be meaningful.
const MIN_DATA_CLASS_ACCESSORS: usize = 3;
/// A class needs at least this many static methods before Utility Class
/// is worth flagging — a single static helper on an otherwise-instantiable
/// class is normal and not what this smell is about.
const MIN_UTILITY_CLASS_STATIC_METHODS: usize = 2;
/// Cyclomatic complexity = decision points + 1. This counts only
/// control-block openings (if/for/while/switch/catch/case, already
/// detected by `opens_control_block`/case-line scanning elsewhere in this
/// file) as decision points — deliberately not `&&`/`||`/`?:`, which would
/// need real expression parsing to count without false positives from
/// string literals or comments containing those characters. This
/// under-counts true cyclomatic complexity somewhat, the same conservative
/// direction as this file's other line-scan-based heuristics.
const DEFAULT_MAX_CYCLOMATIC_COMPLEXITY: usize = 10;
/// A method with many return points is often doing too much conditional
/// branching to be followed easily — same threshold family as the other
/// metrics here (Checkstyle/detekt's own ReturnCount default is also
/// single digits).
const DEFAULT_MAX_RETURNS: usize = 4;
/// detekt's CognitiveComplexMethod default (not on-by-default upstream,
/// but this is the threshold it documents) — cognitive complexity grows
/// faster than cyclomatic complexity for nested code, so its threshold is
/// noticeably higher than `DEFAULT_MAX_CYCLOMATIC_COMPLEXITY`.
const DEFAULT_MAX_COGNITIVE_COMPLEXITY: usize = 15;
/// detekt's ComplexInterface default.
const MAX_INTERFACE_MEMBERS: usize = 10;

/// Every bare `metric > threshold` (or `>=`, for the two accessor/method
/// -count floors below) comparison in this file, YAML-configurable via
/// `kind: threshold` rules (see
/// `crates/core/src/analyzers/threshold_rules.rs`). `data_class_min_accessors`
/// and `utility_class_min_static_methods` sit inside more struct-shaped
/// logic (trivial-accessor ratios, static/constructor detection) than a
/// bare comparison, but the numeric limit itself is still a single value
/// each check reads once — the surrounding boolean gates (`is_java`,
/// `all_methods_static`, ...) stay hardcoded Rust logic regardless, the
/// same way `god_class`/`complex_interface`'s own surrounding `SpanKind`
/// matching already does. `Default` reproduces today's hardcoded behavior
/// exactly, so `detect_complexity_in_file` (used by dozens of existing
/// unit tests) keeps working unchanged.
#[derive(Debug, Clone, Copy)]
pub struct ComplexityThresholds {
    pub cyclomatic_complexity: usize,
    pub too_many_returns: usize,
    pub cognitive_complexity: usize,
    pub long_method: usize,
    pub long_parameter_list: usize,
    pub deep_nesting: usize,
    pub god_class: usize,
    pub rust_god_module: usize,
    pub large_switch: usize,
    pub complex_interface: usize,
    pub data_class_min_accessors: usize,
    pub utility_class_min_static_methods: usize,
}

impl Default for ComplexityThresholds {
    fn default() -> Self {
        ComplexityThresholds {
            cyclomatic_complexity: DEFAULT_MAX_CYCLOMATIC_COMPLEXITY,
            too_many_returns: DEFAULT_MAX_RETURNS,
            cognitive_complexity: DEFAULT_MAX_COGNITIVE_COMPLEXITY,
            long_method: DEFAULT_MAX_METHOD_LINES,
            long_parameter_list: DEFAULT_MAX_PARAMS,
            deep_nesting: DEFAULT_MAX_NESTING,
            god_class: DEFAULT_MAX_CLASS_MEMBERS,
            rust_god_module: DEFAULT_MAX_RUST_MODULE_ITEMS,
            large_switch: DEFAULT_MAX_SWITCH_CASES,
            complex_interface: MAX_INTERFACE_MEMBERS,
            data_class_min_accessors: MIN_DATA_CLASS_ACCESSORS,
            utility_class_min_static_methods: MIN_UTILITY_CLASS_STATIC_METHODS,
        }
    }
}

impl ComplexityThresholds {
    pub(in crate::analyzers) fn set_metric(&mut self, name: &str, value: usize) {
        let metrics = [
            ("cyclomatic-complexity", &mut self.cyclomatic_complexity),
            ("too-many-returns", &mut self.too_many_returns),
            ("cognitive-complexity", &mut self.cognitive_complexity),
            ("long-method", &mut self.long_method),
            ("long-parameter-list", &mut self.long_parameter_list),
            ("deep-nesting", &mut self.deep_nesting),
            ("god-class", &mut self.god_class),
            ("rust-god-module", &mut self.rust_god_module),
            ("large-switch", &mut self.large_switch),
            ("complex-interface", &mut self.complex_interface),
            (
                "data-class-min-accessors",
                &mut self.data_class_min_accessors,
            ),
            (
                "utility-class-min-static-methods",
                &mut self.utility_class_min_static_methods,
            ),
        ];
        if let Some((_, threshold)) = metrics.into_iter().find(|(metric, _)| *metric == name) {
            *threshold = value;
        }
    }
}
