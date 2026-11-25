# Historical benchmark candidates

These candidates need a matching rule and a verified replay before they can
contribute to accuracy scores. They are not counted as detected findings.

## Project-defined Go error returns

- Repository: https://github.com/taigrr/yq, MIT license at the pinned revisions.
- Before: `f8a700e4005dbf47528eaee3060e90ae5d41b5e4`,
  `pkg/yqlib/operator_explode.go:21`.
- Fix: `af39fc737dc49317dafe3adf99fb24749f759482`.
- Evidence: the earlier code calls `explodeNode` without using its error;
  the fix propagates that error. The function is declared in the same file.

This is a candidate for checking discarded error returns from resolved Go
functions, rather than only APIs named by a contract. A future case should
label every finding in the whole file, include clean counterexamples, and
confirm the expected behavior against both revisions before scoring it.
