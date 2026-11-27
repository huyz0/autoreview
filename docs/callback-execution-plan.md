# Callback execution evidence

## Problem

The Kotlin coroutine patterns use lexical nesting to associate an operation
with a suspend function. That is not enough for a lambda: `map { ... }` can
execute its body at the call site, while a functional-interface conversion can
construct a callback for later use. The RediCloud replay in
`benchmarks/redicloud-coroutines.yaml` captures this distinction. Its
`PasswordAuthenticator` lambda is registered with an SSH server; the same
callback is reported in both revisions even though the fix removes the direct
blocking calls. The replay remains intentionally unsuccessful until the rule
can distinguish these cases.

Excluding every lambda would hide the existing `map` positive fixture. Treating
every constructor-like name as a functional interface would also be unsound:
names can be shadowed, and a regular function can invoke its argument while
constructing a value. An imported type is useful evidence, but the current
pattern engine does not resolve its Java declaration or its receiver type.

## Intended behavior

Introduce a shared, bounded execution-context analysis for rules that need it.
For each operation within a callback, classify its relationship to the
enclosing call as **immediate**, **deferred**, or **unknown**. Evidence must
identify the resolved callee, the callback argument, and the reason for the
classification. A rule may use that result to decide whether the operation
belongs to its enclosing execution context; unknown must never silently become
either a definite finding or a clean negative.

Execution context and wall-clock timing are different. RediCloud calls
`start()` before its suspend function returns, so an SSH client could trigger
authentication while that function is still active. That alone does not put
the callback on the suspend function's execution stack. Contracts and their
consumers must describe the execution relationship, not infer it from whether
the enclosing function has returned.

The RediCloud before snapshot pins Apache SSHD 2.17.1. In that version,
[`SshServer.setPasswordAuthenticator`](https://github.com/apache/mina-sshd/blob/sshd-2.17.1/sshd-core/src/main/java/org/apache/sshd/server/SshServer.java)
stores its parameter in a field. The method and class are overridable, so
this implementation alone does not prove behavior for an arbitrary receiver.
The compiler probe test uses the real 2.17.1 classes and checks that the
resolved setter is reported as open. Before using a contract, verify the
concrete receiver, artifact identity, and relevant invocation paths; do not
assume the setter's behavior for a subclass or another version.
The probe also records the receiver's static classifier. In both pinned
RediCloud revisions it is `org.apache.sshd.server.SshServer`; a compiler
fixture with an overriding subclass shows why this does not establish the runtime class
or whether the callback runs in the setter. This metadata remains unverified
and does not affect findings or replay coverage.
For both pinned revisions the probe also observes the `FileCluster.sshd`
property and a setter call whose argument is `SshServer.setUpDefaultServer`.
That list excludes field initializers, direct field writes, reflection and
omitted sources; it is not yet proof of the property's complete value flow.
The compiler reports a private property with default accessors, a null field
initializer, and no other observed direct field writes or accessor references
in either compiled snapshot. The supplied source subset and external report
are not independently authenticated by the review engine, so these facts
still do not justify suppressing the finding.
The pinned RediCloud compiler snapshots also resolve the setter argument to a
zero-parameter external Java declaration returning `SshServer`, with no
dispatch receiver or type parameters; the receiver property belongs to the
same compiled source as the callback. These are still diagnostic facts until
the review process verifies the compiler, inputs, classpath identity and
execution contract itself.
The project also pins Kotlin 2.3.21. The experimental probe supports that
version and 2.2.20 using separately built version-specific registrars; this
does not by itself provide the project's complete source or classpath.

An explicitly configured review can run the snapshot probe itself by setting
`AUTOREVIEW_KOTLIN_CALLBACK_PROBE_CONFIG` to an absolute JSON file. The file
specifies absolute `python`, `kotlinc`, and `plugin` paths, a `jvmTarget`,
snapshot-relative `sources`, compile `jars`, and `runtimeJars`. These programs and the
plugin execute with the user's privileges and must be trusted. The same
relative source list is resolved separately in each replay snapshot. Invalid
configuration or probe failure remains incomplete coverage. The configuration
may also specify `requiredClasses` to hash uniquely present JAR entries; it
cannot be combined with `AUTOREVIEW_KOTLIN_CALLBACK_EVIDENCE`. The runner
can check `sourceRoots` against the source list before and after compilation,
and independently rehashes the sources, dependencies and plugin. Declaring roots does not
prove that they cover the build's actual source sets. Only when the user explicitly sets
`trustDeclaredBuildInputs` to `true`, certifying that the configured source roots and
compile and runtime classpaths describe the actual build, can a narrowly scoped
Apache SSHD 2.17.1 contract suppress a callback finding. It requires the audited
class hashes on both classpaths, no conflicting source or compiled classes, and a private
receiver whose only observed non-null assignment is the audited default factory.
Unverified caller-supplied reports and all other cases remain incomplete. This
opt-in assumes the trusted compiler, plugin and Python runner are authentic,
the declared build inputs are complete, and runtime instrumentation or reflective
mutation does not change those inputs. The runner does not contain descendants.

Initially target Kotlin coroutine checks, but make contracts independent of
the individual rule and language. The model must distinguish a functional
interface value from its later invocation, including immediate invocation of
a newly constructed value and local aliases. It must not assume a library
method is inline, deferred, or harmless solely because its name matches a
known method. Contracts need a resolved declaration identity and argument
position; local declarations, aliases, imports, overloads, type aliases and
shadowing must be tested before an identity is trusted.

## Delivery sequence

The Kotlin pattern coverage check recognizes directly invoked lambda literals
such as `({ ... })()` and `({ ... }).invoke()`. It also follows contiguous
immutable local aliases from a literal to an explicit `.invoke()` call, with a
32-statement limit. A zero-argument bare call is also recognized when the
lambda has no declared parameters and its final local name occurs only in its
declaration and call within the enclosing function. Conflicting local names
and exhausted syntax budgets remain unknown. An unresolved outer lambda is
still reported when an inner lambda is directly invoked. Intervening
statements, mutable bindings, other bare-name calls, inline library methods,
and functional-interface conversions remain unresolved.
An immutable local lambda with no other matching identifier in its enclosing
function is also recognized as uninvoked, within a shared per-file syntax-node
budget. Same-named shadowed identifiers conservatively prevent that proof.

1. Add an execution-context result to the shared analysis model, including
   source and callback locations, evidence identity, and an explicit unknown
   reason. Define path and analysis budgets and propagate exhaustion as
   coverage, scoped to the affected rule and function.
2. Resolve a bounded Kotlin subset: named functions and anonymous functions,
   directly invoked lambdas, supported local aliases, and uniquely resolved
   functional-interface conversions. Treat dynamic dispatch, ambiguous imports,
   unresolved classpaths, escaping values, and unsupported syntax as unknown.
   Do not infer that a stored callback is never invoked before the function
   returns without proving the relevant call paths.
3. Add versioned, declarative contracts for known APIs, with language, resolved
   symbol identity, callback parameter, execution timing, and provenance.
   Validate contracts at load time and use the same definitions for bundled and
   external packs. Keep framework-specific assumptions out of generic syntax
   matching. Begin with a verified Apache SSHD registration contract and a
   verified Kotlin inline operation; expand only with evidence and negatives.
4. Connect the Kotlin coroutine rules to execution evidence through a supported
   rule-engine feature, preserving stable rule IDs. A raw ast-grep invocation
   cannot evaluate the extra semantic condition: document that limitation and
   make Autoreview's behavior and exported rule metadata explicit rather than
   silently changing one execution path. Review, replay, and gating must share
   the same condition and incomplete-analysis behavior.
5. Extend only after parity tests establish equivalent semantics for other
   supported languages. Java and Go callbacks, JavaScript/TypeScript/TSX
   handlers and promises, and Rust closures differ in dispatch, ownership, and
   scheduling; each needs its own resolution and execution evidence.

## Required verification

- Exercise direct calls, inline `map`, a stored callback, an immediately
  invoked callback, nested callbacks, and callbacks invoked through a local
  alias. Include early returns, branches, reassignments, and callbacks whose
  execution is conditional.
- Exercise shadowed names, unrelated same-name APIs, renamed and wildcard
  imports, ambiguous dependencies, conflicting declarations, overloaded calls,
  unknown receivers, unsupported expressions, and exhausted budgets. None may
  be treated as verified solely because a name resembles a known API.
- Replay the RediCloud before and after snapshots. The two direct calls should
  remain findings; the registered authenticator should not be attributed to
  the enclosing suspend function. The case must complete with no mislabeled
  findings or hidden coverage limitations. Preserve a separate fixture for an
  immediately invoked authenticator so the same callback body is not always
  suppressed.
- Verify the other three Kotlin coroutine rules as well as the blocking rule;
  exercise every built-in positive and negative fixture and run the full rule
  conformance suite. Add CLI review and replay tests for both completed and
  incomplete cases, including changed lines and an unknown callback identity.
- Run historical replays, Autoreview on the final diff, and hosted CI. Record
  measured results and remaining limitations; one passing fix pair cannot
  establish general precision or recall.

## Open design decisions

An experimental standalone compiler-IR probe lives in
`tools/kotlin-callback-evidence`. Its caller-supplied reports remain unverified
diagnostics. The review-owned runner and narrowly scoped opt-in SSHD contract
are described above. General dependency discovery, compiler and JDK identity,
process containment, overload identity, and additional execution contracts
remain open.

The repository has no general Kotlin classpath-backed resolver. A syntactic import alone is insufficient
when local or dependency declarations can shadow it. Establish how such a
probe behaves on unavailable dependencies and snapshots, including whether it
executes project tooling, before making it a default.

The ast-grep post-processing path attaches structured coverage to uncertain
matches, and review, replay, and gates share it. Additional contracts must use
the same path; a CLI-only filter must not silently score unresolved callbacks.
