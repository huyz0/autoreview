# Improvement delivery plan

This plan starts from main commit 0647f09. Each change requires review, appropriate
tests, Autoreview, and passing CI before merge. The acceptance criteria below
describe intended results, not completed work.

## 1. Track ignored results across helpers

Reuse the call-sequence resolver for same-file and imported functions. Build
per-function summaries that distinguish an argument read, an argument returned,
and an ignored argument. Propagate return demand from each caller to the helper
and preserve the source file and call location of each result. Summaries must
converge for recursive call graphs, preserve branch-dependent reassignments,
and treat unresolved dispatch, missing sources, and unsupported expressions
conservatively. Keep separate identities for different files and invocations.

Verify Go, Java, Kotlin, Rust, JavaScript, TypeScript, and TSX through the
normal review and replay entry points. Include multi-hop imports, aliases,
forwarded and ignored parameters, ignored helper returns, mixed callers,
recursion, shadowed imports, exceptions, and prior local and closure cases.
Findings must identify the original acquisition and avoid duplication.

### Helper-flow audit checkpoint

The conformance tests exercise imported helper arguments and returned values
for Go, Java, Kotlin, Rust, JavaScript, TypeScript and TSX. Web re-exports and
several Rust module and re-export forms have additional cases. These are
specific fixtures, not proof of all combinations in the acceptance criteria.
The CLI suite exercises direct imported-helper arguments and returned sources
through both review and replay in all seven supported language grammars. Replay
labels can name a caller as `entry` while scoring the source file via `path`.
These cases do not establish parity for every helper shape or combination.

An ambiguous web re-export is now reported even when no known result source
is registered. CLI regressions check that review exposes the structured
limitation and replay excludes the incomplete case from scoring. Rust calls
such as `Type::method()` through an imported type remain a separate gap:
the helper resolver does not generally model associated functions. Its
unresolved-import warnings must remain until supported declarations and
dispatch can be identified without guessing. Direct imports and aliases of a
non-generic local struct or enum can now resolve unconditional, non-generic
inherent associated functions with `pub` or `pub(crate)` visibility. The
current coverage warning remains conservative even for these supported calls.
Same-file calls to supported private or public inherent functions also trace
results through `Self::` forwarding; review and replay have regression tests.
Shadowed type names, local imports and unresolved same-file associated calls
are reported as incomplete rather than assigned a guessed target.
Trait dispatch, generic types and implementations, conditional compilation,
re-exported types and ambiguous declarations need further work.

Results reaching an unresolved receiver now produce a coverage limitation
instead of implying the receiver was verified. Supported flows include direct
arguments, local aliases, branches, tracked fields and closures, and eligible
helper arguments and returns across files. The analysis does not infer whether
arbitrary external APIs actually consume a value; unmodeled syntax, escapes,
and ambiguous dispatch remain limits on this coverage.

## 2. Measure rule accuracy

Extend the existing labelled Git replay workflow with documented, reproducible
cases from real fixes and clean versions in representative projects. Record
source revisions, rule and language, expected locations, and the reason for
each label. Separate historical cases from synthetic regression fixtures.
Report true positives, false positives, false negatives, precision and recall
where denominators exist, plus failures and skipped analysis by rule and language.
Check corpus licensing and avoid claiming population-wide accuracy from a
small sample. Run the corpus against the current baseline and use the results
to rank subsequent rule changes.

## 3. Report coverage and skipped analysis

Inventory every early return and analysis budget in the control-flow and
contract engines. Carry structured reasons and locations into review reports
for unsupported syntax, unresolved imports, analysis limits, missing tools,
and errors. Deduplicate warnings and distinguish skipped files from individual
functions or rules. Preserve existing JSON and SARIF compatibility. Test that
an affected diff reports the relevant limitation and that supported code does
not acquire a warning. Measure frequency in the benchmark before choosing
which unsupported constructs to implement next.

## 4. Improve high-value gaps

Use the benchmark and coverage report to select concrete missed cases. Assess
floating promises, null and bounds checks, injection source and sink coverage,
and resource lifecycles across helper boundaries. For each change, identify
the API or type evidence, add real positive and safe negative cases, measure
regressions against the baseline, and document supported language and framework
shapes. Exercise query-count fixtures before broadening implicit ORM N+1
findings.

### Project-defined Go error results

The `taigrr/yq` candidate discards errors at three call sites in one file;
see `benchmark-candidates.md` for pinned revisions. First establish a trusted
Go type-checking path that identifies the resolved callee and its actual result
types. A spelling-based `error` annotation is insufficient because declarations,
imports, and build constraints can change its meaning or the selected function.
Do not infer a package-wide fact from the current helper loader: it deliberately
filters files that do not appear to supply helpers.

The experimental `typed-go-error` source connects the standalone probe to the
result-use engine through an explicitly configured trusted toolchain. One
historical yq replay detects three discarded errors and none after the fix.
The checker reports unavailable or unmodeled evidence and is not a complete
replacement for Go tooling. Its README records package, snapshot, and process
containment limits.

The experimental rule tracks single-error calls and independent error positions
in supported multi-result assignments; a used non-error result does not hide a
discarded error. Continue preserving source identities through callers and
helper summaries. Sole multi-result arguments are tracked through uniquely resolved helpers;
unknown receiving helpers are reported as incomplete. Model explicit `_`,
`defer`, `go`, aliases, reassignment, shadowing, generics, method and interface
dispatch conservatively. Record incomplete type checking and unsupported call
shapes against the affected rule and file, rather than counting them as clean.

Use the three yq lines and the entire corrected file as one historical pair,
add independent fixes and clean examples, and check build tags, package and
import ambiguity, changed-line reporting, and unavailable dependencies. Compare
the replay and runtime against the current baseline before enabling the rule;
this single pair cannot establish general precision or recall.

### Checked errors and cancellation

The checked-error-to-nil heuristic has a known Cilium cancellation false
positive. Its limiter's `Wait` calls `WaitN(ctx, 1)`; `WaitN` has a separate
burst-exceeded error, ruled out at that call site by the local burst-one
constructor. A method named `Wait` is not evidence that errors are limited to
cancellation. The static rule must continue warning unless the receiver,
configuration, and possible error paths are established.

The experimental Go probe can optionally identify a statically bound callee;
the Go result-use rule can attach it to finding metadata without changing
the finding decision. Before using it to suppress
a warning, negotiate probe capability without breaking older probes; validate
call locations and source identities; trace the actual receiver and constant
constructor arguments; and compute conservative summaries of all reachable
error paths through the relevant source and dependencies. Unknown interface
dispatch, mutable receiver state, missing code, unsupported control flow,
changed snapshots, or incomplete dependencies must keep the warning. Test an
unrelated `Wait` implementation and a limiter whose request exceeds its burst,
then rerun the pinned Cilium positive and cancellation cases along with other
independent real fixes. Optional model verification is separate and has not
been measured against this benchmark.

## 5. Release readiness

Exercise install, doctor, a representative diff review, replay, and SARIF on
the supported release platforms. Verify checksums, release assets, and the
Homebrew formula against the release workflow. Decide Windows binary support
from an actual Windows packaging and signing assessment. Update installation
and version documentation to match the shipped assets, then produce a
release candidate after the changes above pass. Record any platform that
could not be exercised and the evidence needed to clear it.

## Delivery checks

Review each PR on its final head, resolve actionable findings, and verify CI
on that head before merging. Preserve unrelated local changes. Record benchmark
results and unresolved analyzer coverage limitations with the corresponding
change so reviewers can reproduce the decision.
