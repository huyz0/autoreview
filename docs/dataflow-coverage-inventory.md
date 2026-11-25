# Dataflow coverage inventory

This inventory records paths that can make a clean dataflow review incomplete.
It does not claim that every unsupported construct has been catalogued.

| Condition | Current behavior | Evidence to report |
| --- | --- | --- |
| Changed or imported source cannot be read or parsed | `ParseCache::get` returns `None`; its callers may skip the file. | Source path, whether it was an entry or dependency, and failure reason. Deleted files need separate treatment. |
| Cross-file call-sequence discovery reaches its 64-dependency limit | The resolver stops loading sources. | Entry scope, limit, and whether an unvisited dependency remains. Do not warn merely because the queue contains already loaded duplicates. |
| Keyed lowering exceeds the finally-path copying limit | `keyed_flow` reaches zero and `Plan::new` declines the function. | File, function location, and affected call-sequence analysis. |
| Keyed path exploration exceeds its state budget | The root analysis returns no result. | Root location, rule, and path-budget reason. Distinguish this from unsupported identity or call effects. |
| Ignored-result helper summaries do not converge within their iteration budget | All helper summaries for that rule are cleared. | Rule and analyzed source scope; ordinary local checking can still run. |
| Local handles, cleanup identities, closures, calls, or syntax cannot be modeled | Several conservative `None` paths decline a plan, root, or expression. | Precise reason and location where available. Do not label ordinary nonmatches as skipped analysis. |
| A keyed cleanup argument contains an unsupported nested expression | Direct nested calls such as `root.remove(choose(key))` are followed, but conditional and other complex argument expressions may still be unmodeled. | Track the returned identity through the expression or report that this path was not modeled. |
| External static tool is unavailable or fails | Review reports include structured analyzer-scoped issues for unavailable ast-grep and golangci-lint and for execution errors from ast-grep, golangci-lint, and Clippy. Clippy findings remain available even when another target fails. Cargo audit errors identify the affected lockfile. Existing text warnings remain. | Other tools and partial failures still need structured coverage; do not imply that other analyzers failed. |

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
