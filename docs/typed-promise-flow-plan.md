# Typed promise flow: implementation plan

The historical SFTPresso case in `benchmark-candidates.md` remains unscored.
Its `uploadFile` and `removeRemote` bindings are produced by a factory whose
TypeScript return annotation is a function returning `Promise<void>`. The
barrel re-exports are resolvable, but the returned function type and discarded
calls are not yet modeled. The implementation must establish those facts from
the source; matching the names of the factory or helpers is insufficient.

An exploratory parse of the pinned before revision confirms that
`createFileHandler` declares a returned callable with one required and one
optional argument, and that `removeRemote` is an immutable binding initialized
by a one-argument call to that factory. This verifies the syntax probe only; it
does not resolve the cross-file binding or score the historical case.

## Deliverables

1. Represent proven promise-returning callable signatures as reusable facts.
   Support explicit TypeScript `Promise<T>` return annotations, async
   declarations, and factory return annotations of the form
   `(...) => Promise<T>`. Preserve the declaration and source location that
   justify each fact. Distinguish a function returning a promise from a
   factory returning such a function.
2. Resolve those facts through immutable bindings, imports, aliases, named and
   wildcard re-exports, and supported helper returns. Respect export
   ambiguity, shadowed names, optional and rest parameters, reassignment,
   cycles, and the existing dependency and resolution budgets. Do not infer
   a fact from an unresolved module, a mutable binding, a conflicting export,
   or a locally shadowed `Promise` type.
3. Feed proven promise sources into ignored-result analysis, so a bare call,
   an unused assignment, and values passed through modeled helpers are checked
   consistently. Awaiting, returning, and otherwise observing the same value
   must remain clean. Preserve distinct call origins and changed-line related
   locations.
4. Cover promises discarded from callbacks used by `Promise.all` and related
   combinators, including callbacks with a `try`/`catch` around an unawaited
   call. Only report when the promise source is proven and the callback does
   not return or await that value on the relevant path. Exercise branches,
   nested callbacks, explicit `void`, reassignment, and synchronous
   counterexamples; do not assume every call in a map callback is asynchronous.
5. Report unsupported or budget-exhausted analysis at the affected scope and
   prevent replay from scoring that scope as clean. Keep unrelated files and
   rules usable, and preserve the existing compatibility of report consumers.

## Evidence and release gate

- Add positive, negative, ambiguity, shadowing, and regression fixtures for
  JavaScript, TypeScript, and TSX where the type facts are actually available.
  JavaScript must not be assumed to have TypeScript annotations; async syntax
  and explicit supported contracts can supply evidence there.
- Replay both pinned SFTPresso revisions with labels reviewed for the whole
  file. Record any remaining incomplete analysis instead of scoring it. This
  one case cannot establish project-wide precision or recall.
- Exercise helper summaries, closure and branch flow, and changed-line
  filtering alongside the new rule. Compare performance on representative
  repositories and retain bounded traversal with reported exhaustion.
- Run Autoreview and CI on the exact PR head, resolve findings, and merge only
  after the reviewed head is green. Factory-return inference and a passing
  historical replay are separate requirements; neither alone completes this
  plan.
