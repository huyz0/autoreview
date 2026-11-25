# Historical benchmark candidates

These candidates need a matching rule and a verified replay before they can
contribute to accuracy scores. They are not counted as detected findings.

## Project-defined Go error returns

- Repository: https://github.com/taigrr/yq, MIT license at the pinned revisions.
- Before: `f8a700e4005dbf47528eaee3060e90ae5d41b5e4`,
  `pkg/yqlib/operator_explode.go:21`, `:68`, and `:72`.
- Fix: `af39fc737dc49317dafe3adf99fb24749f759482`.
- Evidence: the earlier code calls `explodeNode` without using its error;
  the fix propagates that error. Two further calls discard `applyAlias` errors;
  the same fix propagates those too. Both functions are declared in the file.

The experimental typed Go rule now detects all three calls in the historical
replay and none in the corrected file; see `benchmarks/yq-error.yaml`. This one
fix pair does not establish general precision or recall, and the checker remains
opt-in with documented package and runtime limitations.

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

The typed factory rule now detects this case with an explicitly configured,
trusted TypeScript compiler. The normal replay and its limits are documented in
`benchmarks/sftp-promise.yaml` and `benchmarks/README.md`. One fix pair cannot
establish general precision or recall.

## Ordinary TypeScript promise-returning calls

- Repository: https://github.com/actions/checkout, MIT license at both revisions.
- Before: `c952173edf28a2bd22e1a4926590c1ac39630461`.
- Fix: `5a4ac9002d0be2fb38bd78e4b4dbde5606d7042f`.
- Labels: `src/github-api-helper.ts:50` and `:73` discard the promises from
  `io.rmRF`; `src/git-auth-helper.ts:151` discards the promise from a class
  method. The fix awaits all three. The commit explains the practical race
  caused by the discarded class-method result.
- The two revisions have identical `package.json`, `package-lock.json`, and
  `tsconfig.json`. A local experiment installed the fixed revision's locked
  dependencies with `npm ci --ignore-scripts --no-audit --no-fund` and used a
  separately configured trusted TypeScript compiler. Snapshots were placed
  beneath the checkout so their imports could resolve the installed packages.
- The experimental factory-promise rule reported no findings on either file.
  This is expected from its documented factory-only scope and must not be
  counted as a false-negative score for that rule. The cases are candidates
  for a general typed promise-result rule, not completed benchmark cases.

To turn these into scored cases, the general rule needs to prove the return
type of ordinary imported functions and class methods, track the particular
returned promise through the existing result-use engine, and report missing
type evidence rather than infer from a method name. Verify both fixed files
and unrelated synchronous or intentionally observed calls as negatives;
preserve compiler and source-snapshot checks, bounded execution, and coverage
reporting. Add the pinned files as separate replay cases only when that rule
exists, and check all findings in each entire file before accepting its labels.
