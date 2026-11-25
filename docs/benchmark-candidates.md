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

## Promise-returning TypeScript helpers in `Promise.all`

- Repository: https://github.com/jmwerk/SFTPresso, MIT license at the pinned revisions.
- Before: `be43f28401e0ba693e9bfe0a1b27476d9eb4117a`,
  `src/commands/commandUploadChangedFiles.ts:100` and `:126`.
- Fix: `460fa47433bf6ed423cc84cb0939ce6b90246e6a`.
- Evidence: both callbacks call a promise-returning imported helper without
  returning or awaiting the promise. `Promise.all` therefore receives
  callbacks that return `undefined`, and their surrounding `catch` blocks
  cannot catch later rejections. The fix awaits each helper in an async callback.
  The helpers are exposed through a barrel export and created by a factory
  whose return type is `Promise<void>`.

This is a candidate for type-aware promise tracking across imports, re-exports,
and function factories. Validate the complete file and safe alternatives before
using it to score a rule; matching the helper names alone is not evidence that
arbitrary calls return promises.
