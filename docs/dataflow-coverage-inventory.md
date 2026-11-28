# Dataflow coverage inventory

This inventory records paths that can make a clean dataflow review incomplete.
It does not claim that every unsupported construct has been catalogued.

The four Kotlin coroutine pattern rules also report
`callback_execution_unresolved` when a matched operation occurs inside a
lambda within a suspend function, except when direct literal invocation or a
contiguous chain of immutable local aliases followed by `.invoke()` proves it
immediate, and every enclosing lambda is also proved immediate. The finding is retained with reduced
confidence because the callback may execute inline or later. Replay and gates
exclude the affected rule's incomplete scope from verified results. This is
limited to existing pattern matches: it does not inventory unmatched syntax,
resolve callback identities, or classify other callback forms as immediate or deferred.
Alias tracking is limited to 32 subsequent statements, including comments;
reassignment, control flow, intervening calls, and bare-name invocation remain
unresolved.
An immutable local lambda with no other matching identifier in its enclosing
function is treated as uninvoked, including when it contains nested callbacks.
This conservative check counts same-named shadowed identifiers as uses and has
a shared per-file budget of 200,000 syntax nodes; exhaustion remains incomplete.

Result-use analysis checks partial observation for directly tracked local
results on bounded, acyclic paths with supported simple guards. It preserves
correlated, stable guards, so `const p = task(); if (flag) await p;` is
reported while a result both created and observed under the same guard stays
clean. Path-budget exhaustion is reported as `result_use_path_limit`.
Other cases, including loops, caught exceptions, closures, helper effects,
complex or unstable guards, and nonlocal storage, still use the older merged
liveness analysis and may miss a result observed on only one path. When a
local source is treated as used but cannot be proved observed before the
unsupported flow, `result_use_path_unmodeled` reports incomplete analysis.
The same warning can identify a locally tracked result or a direct result
passed as an argument to a resolved
helper that may leave its parameter unobserved, unless subsequent caller paths
prove the result is used. Helper guard values are not yet mapped to caller
arguments in general, so this is an incomplete-analysis warning, not a proven defect.
When a supported helper has one stable parameter guard and the caller passes a
literal boolean or a directly initialized, unreassigned boolean local, Java,
Kotlin, JavaScript, TypeScript, TSX, and Rust can
specialize that guard and report a definite unused result or establish that the
helper observes it. Go retains the warning because its predeclared boolean
names can be shadowed and that binding is not yet verified for this check.
For bounded acyclic callers with supported effects, the same helper guard can
also be correlated with a stable caller branch or alias. This can report a
discarded result on a proven path; unknown effects and unstable guards remain
incomplete. When such a helper returns a tracked result into a local binding,
supported paths also follow the returned value through copies, observation,
return, and reassignment. A locally assigned result no longer receives the separate
partial-observation warning when every reachable path is proved observed;
unobserved paths and exhausted path budgets do not receive that exemption.
Stable boolean alias chains are followed for up to 64 bindings. Writes or
mutable borrows anywhere in the chain prevent specialization, even when they
occur after an alias is copied. Longer chains and path-dependent assignments
remain incomplete.
This check does not cover every storage shape, helper chain, or expression.
Merely intersecting liveness sets would produce false positives for correlated guards.
Discarded conditional arms that reference a tracked binding are modeled when
the conditional test and arms are supported. An unsupported conditional can
also produce `result_use_path_unmodeled` when it may hide a local result use;
a call directly in an unsupported arm is reported separately.

| Condition | Current behavior | Evidence to report |
| --- | --- | --- |
| Changed or imported source cannot be read or parsed | Entry files report `source_unavailable`; cross-file call-sequence dependencies report `dependency_unavailable`. Go and Java/Kotlin candidate helper-summary scans report rule-scoped `summary_dependency_unavailable` or `summary_dependency_syntax_errors`. The Java/Kotlin helper package index parses candidate declarations: unavailable sources or directories produce `package_index_unavailable`, malformed declarations produce `package_index_syntax_errors`, and unsupported package names produce `package_index_unresolved`. Caller and same-directory helper package matching also use syntax; unsupported caller packages produce `entry_package_unresolved`. A file's package cannot safely be inferred from its path. The Git review path excludes absent diff paths only when the snapshot's HEAD confirms they were deleted. A failed Git check remains an entry warning. | Discovery still skips configured excluded directories and symbolic links. Import and other package extraction paths remain text-based, and legacy same-package summaries scan only the current directory. |
| Cross-file call-sequence discovery reaches its 64-dependency limit | The resolver stops loading sources. | Entry scope, limit, and whether an unvisited dependency remains. Do not warn merely because the queue contains already loaded duplicates. |
| Keyed lowering exceeds the finally-path copying limit | `keyed_flow` reaches zero and `Plan::new` declines the function. | File, function location, and affected call-sequence analysis. |
| Rust function contains control flow the lowerer cannot model | The lowerer skips the function body; review reports `unsupported_rust_control_flow` per applicable rule and replay treats the case as incomplete. | File, function location, and affected rule. Entry functions and imported helpers reachable through supported calls are included. |
| Keyed path exploration exceeds its state budget | The root analysis returns no result. | Root location, rule, and path-budget reason. Distinguish this from unsupported identity or call effects. |
| Ignored-result helper summaries do not converge within their iteration budget | All helper summaries for that rule are cleared. | Rule and analyzed source scope; ordinary local checking can still run. |
| Local handles, cleanup identities, closures, calls, or syntax cannot be modeled | Several conservative `None` paths decline a plan, root, or expression. For must-complete contracts, `unmodeled_contract_handle` reports a matching acquisition in a reachable function declined because of unsupported storage or transfer. This does not prove the acquired handle is what escaped. | Report other declined plans with a precise reason and location where available. Do not label ordinary nonmatches as skipped analysis. |
| A keyed cleanup call is absent from the lowered graph | Explicit call-sequence and must-complete contract rules report `unmodeled_cleanup_call` for the first matching call in a reachable declined function when a registration is present. Direct nested calls such as `root.remove(choose(key))` are followed. Standalone, returned, and simple local-assignment JavaScript, TypeScript, and TSX conditionals with simple boolean tests and direct calls, identifiers, or literals are lowered into separate paths; destructuring, multiple declarators, and more complex expressions remain unmodeled. | Track the remaining cleanup expressions. Other reasons a plan can be declined still need separate coverage. |
| A result-producing call is in an unmodeled conditional | Direct calls in the arms of unsupported standalone, returned, or assigned conditional expressions report `unmodeled_conditional_result` for applicable result-use rules. The supported conditional form has bounded, side-effect-free boolean tests and modeled arguments; unsupported tests or arms remain incomplete. | Extend expression modeling without treating unsupported branches as clean. Other result expression shapes still need separate coverage. |
| External static tool is unavailable or fails | Review reports include structured analyzer-scoped issues for unavailable ast-grep and golangci-lint and for execution errors from ast-grep, golangci-lint, and Clippy. Clippy findings remain available even when another target fails. Cargo audit errors identify the affected lockfile. Existing text warnings remain. | Other tools and partial failures still need structured coverage; do not imply that other analyzers failed. |

On Windows, a static review of the pinned JPlag trim fix reported no
`summary_dependency_*` limitations, and the selected JPlag and Go tools
replays retained their prior results. These two cases are too few to estimate
the frequency of the new warnings across projects.
Static reviews of the pinned JPlag and RediCloud fixes also reported no
`package_index_unavailable` limitations; this does not establish their
frequency across other repositories.
After parsing unclassified sources, one Windows debug run on each pinned diff
reported zero `package_index_*` issues. The previous-main and modified builds
took 7.02 and 6.43 seconds for JPlag, and 9.61 and 8.17 seconds for RediCloud,
respectively. These sequential single runs include checkout and other analyzers;
they are not a reliable estimate of typical performance or warning frequency.
After switching the helper index to parsed package declarations, single
Windows debug reviews of the same diffs took 7.91/7.62 seconds before/after
for JPlag and 10.82/10.76 seconds for RediCloud, with no package-index warnings.
These end-to-end timings include other analyzers and do not establish general
performance or accuracy.
After extending syntax-based package matching to callers and same-directory
helpers, single Windows debug reviews of those diffs took 7.97/7.59 seconds
before/after for JPlag and 12.41/9.97 seconds for RediCloud. Neither produced
the new package-resolution warnings. These are end-to-end single runs, not a
general performance or warning-frequency estimate.

The experimental Go error probe distinguishes package or call-site budget
exhaustion, package loading, type checking, snapshot mismatches, and rejected
requests in coverage, scoped to a repository-relative package directory when
available. Unknown or malformed probe responses and process failures retain
the generic rule-scoped incomplete-checker reason. These reports do not expose
raw subprocess errors.

Rust lowering supports discarded statement matches, function-tail matches,
direct `return match`, and matches used as simple `let` initializers.
Supported matches require a final wildcard. Scrutinees can
include simple field paths and supported direct calls. Arms can use literals,
qualified unit variants, nested tuple variants, reference bindings, and
alternatives that introduce no names. Arm-local names receive distinct identities, but the analyzer
does not connect them to the matched value or resolve whether an identifier
pattern denotes a constant: those cases report `rust_match_binding_projection`
and replay remains incomplete. Arms can contain supported local assignments
and statement-position conditionals, including early returns before an arm
produces its value. The final wildcard must be unguarded.
Returned arms preserve result identity on each exit. Guards preserve the
evaluation order of `&&`, `||`, and `!` and carry effects to the next arm
when the guard fails. Let chains in a match guard expose names to later
operands and the selected arm. The next arm resolves names in its own scope.
Matches used in other nested value positions, struct and slice patterns,
alternatives that bind names, and guards whose operands need additional lowering still
produce coverage diagnostics. [Rust match expressions](https://doc.rust-lang.org/reference/expressions/match-expr.html) [Rust lazy boolean operators](https://doc.rust-lang.org/reference/expressions/operator-expr.html#lazy-boolean-operators)

Rust `let-else` lowering represents both pattern outcomes when the
initializer is supported and the fallback ends with `return`, `break`, or
`continue`. The fallback sees the outer bindings. Names introduced on the
success path have distinct identities and report
`rust_let_else_binding_projection` because their relationship to the
initializer is unresolved. [Rust let statements](https://doc.rust-lang.org/reference/statements.html#let-statements)

Rust statement-position `if let` and `while let` can use simple patterns
and let chains. Chain operands execute from left to right. Each successful
pattern exposes its names to subsequent operands and the success body.
Failure goes to the original `else` scope or exits the loop. Bindings
whose source values cannot be projected report
`rust_conditional_binding_projection`. Simple value-producing
`if` and `if let` expressions also preserve the selected arm when they
form a function return or initialize a single `let` binding. [Rust if expressions](https://doc.rust-lang.org/reference/expressions/if-expr.html)

Rust `for` and ordinary `let` statements can destructure tuples with
named bindings, wildcard elements, and nested tuples. Loop bindings receive
fresh identities each iteration. Tuple components report
`rust_destructuring_binding_projection` because their relationship to the
initializer or iterator item is unresolved. When a supported `if` or `match`
supplies the tuple, the analyzer follows calls through the selected arm,
including calls in tuple elements and in supported eager unary and binary
operands. Explicit exits leave before the new bindings take effect. [Rust operand evaluation order](https://doc.rust-lang.org/reference/expressions.html#evaluation-order-of-operands)

Report scopes should distinguish an entire file, a function or root, a rule,
and an imported dependency. Findings from unaffected scopes remain useful.
Deduplicate repeated limitations across callers while retaining enough location
information to identify the affected code. Replay and CI gates must not count a
skipped scope as a clean negative example.

The JSON review report now includes an optional `analysisCoverage` array for
read and syntax problems, dependency discovery limits, finally lowering,
path-state exhaustion, skipped paths, and helper-summary convergence limits.
Go discovery also reports unreadable package files and files whose package
cannot be identified, including files that cannot be bound to an import.
Rust discovery skips an external `mod` declaration when a successfully parsed
file contains no identifier reference to that module outside external module
declarations. References in imports and macro arguments are retained, and a
file with syntax errors falls back to loading its modules. This reduces the
chance that unrelated files consume the dependency budget; it does not model
code generated by macros. On the Autoreview diff from `56b2a12` to `14e6ff8`,
one local static review previously reported two dependency-limit warnings;
with this filtering, neither appeared and the same review had no deterministic
findings. Four unresolved-import warnings remained. This is one observation,
not a general performance or completeness measurement.

Rust discovery reports a used `crate::`, `self::`, or `super::` import that cannot be resolved
through an unambiguous chain of ordinary external module declarations. This
includes directly called imported helpers, conditional imports, `#[path]`, and ambiguous crate roots; other Rust
import shapes remain outside this diagnostic. Supported named and wildcard
`pub use` chains are followed; unresolved or unsupported exports reached by a
helper call are reported as incomplete.
For an immediate child using a parent's private module import, discovery also
follows a single unconditional import to a verified module file. Conditional,
ambiguous, wildcard, and unsupported imports remain unresolved. On the same
Autoreview diff, name-specific diagnostics showed eight unresolved names before
this change and six afterward; the two resolved names were private parent
module imports. The six remaining names were associated calls on imported types.
This single comparison does not establish general recall or precision.
JavaScript and TypeScript discovery reports unresolved relative code imports
with runtime bindings. Side-effect, type-only, asset, and bare-package imports
are outside this particular diagnostic.
Named and wildcard web re-exports are followed across files. Unresolved or
unsupported re-exports and reachable ambiguous helper targets are reported;
cycles are handled, and resolution has step and depth limits that are also
reported. Synchronous top-level `const` arrow functions and function
expressions, including direct default exports, can be resolved as helpers.
Namespace re-exports, mutable or async function values, and function values
returned by factories are not resolved as named helpers.
Entries identify the analyzer, reason, scope, and available path, line, and
rule. The same issues are summarized in `analyzerWarnings` for text reports.
Dataflow replay fails affected cases instead of treating them as clean. Other
unresolved imports and unsupported expressions are not yet fully reported, so
an empty array is not a guarantee of complete coverage.

For JavaScript, TypeScript and TSX, a registered result directly wrapped in an
array or spread passed to a call is reported as `wrapped_result_receiver`.
The analysis does not yet follow individual array elements into callees, even
when a helper resolves; the warning can therefore appear for a receiver that
actually handles every element. An awaited result is not treated as an
unobserved promise by this check. Results assigned before being placed directly
in an array or spread argument are also tracked through supported aliases and
helper calls and reported under `unresolved_result_receiver`. Simple array
literals can also be stored and aliased before a call, including arrays returned
or passed through supported helpers. Known elements remain tracked when another
element uses an unsupported expression; direct calls to supported helpers in
array literals are followed. This does not model array mutations, every element
expression, or all collection APIs.
