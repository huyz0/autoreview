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
