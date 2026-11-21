# Antipattern coverage

Autoreview can report a concrete instance of a broad antipattern when it has enough evidence in the diff and the surrounding code. This inventory records the scope of those checks and the next testable case for each family. Rule IDs are representative. JavaScript and TypeScript are listed as web languages, and TypeScript patterns also run on TSX.

A syntax rule can match a specific API sequence or local expression. Data flow is needed when a value or obligation crosses assignments and branches. Graph checks need resolved dependencies or an explicit architecture policy. Production behavior and design intent require evidence such as traces, query plans, or change history.

## Code design and complexity

| Antipattern | Autoreview coverage | Feasible rule or evidence needed |
|---|---|---|
| Long method, long parameter list, too many exit points, deep nesting, high cyclomatic or cognitive complexity | `long-method`, `long-parameter-list`, `too-many-returns`, `deep-nesting`, and both complexity thresholds. | Use language-specific counting and keep thresholds configurable. Compare branching functions with long declarations or generated tables before extending a metric. |
| God class, god module, low cohesion | `god-class` and `rust-god-module` use structural metrics. | Other languages can combine module size, public surface, and weakly connected members once reference indexing is available. Treat the result as advisory and exercise facades as negatives. |
| Feature envy, inappropriate intimacy | `feature-envy` uses the symbol index where available. `rust-feature-envy` recognizes repeated access through a typed parameter in an inherent method. | Extend receiver resolution for aliases and helpers. Keep delegation, serialization, adapters, and intentional builders in the negative corpus. |
| Data clumps, primitive obsession | The symbol index reports recurring parameter groups with `data-clump`. Kotlin also flags long positional argument sequences. | Units and domain meaning need types or an explicit policy to distinguish IDs and amounts from ordinary primitives. |
| Message chains, Law of Demeter | `message-chain` reports long chains using the symbol index. | Receiver types can distinguish navigation across domain objects from fluent APIs, builders, and immutable collection transformations. |
| Refused bequest, fragile base class | The symbol index reports `refused-bequest` for supported inheritance patterns. | When class and method identities are known, flag a constructor calling an overridable method on its own instance. Verify final, private, and static methods are excluded. |
| Data class, anemic domain model | `data-class` is a structural class metric. | Domain policy can exempt DTOs, projections, and persistence entities. The model alone cannot establish where business behavior belongs. |
| Fat interface, inappropriate interface shape | `complex-interface`, `go-empty-interface`, and `java-interface-is-type`. | An advisory unused-capability check needs interface implementations and clients. Generated service clients and extension interfaces are important negatives. |
| Middle man, speculative generality, premature abstraction | No dedicated check. | Surface a single-client pass-through abstraction with a resolved call graph. Keep framework adapters and published extension points out of generic enforcement. |
| Large switch, control flags and convoluted branches | `large-switch`, nesting and complexity thresholds, JVM nested-conditional patterns, Kotlin sealed catch-all check. | Find Boolean flags whose sole purpose is to carry a loop exit or branch condition. Require a use-def proof and exclude flags consumed outside the loop. |
| Copy-paste, divergent implementations, semantic duplication | `duplicate-code-block` and `duplicate-code-block-cross-file` report repeated text blocks. | Normalize ASTs for near-identical nontrivial code. Flag equivalent behavior only with additional evidence, and exclude generated clients and common setup scaffolding. |
| Dead code, unreachable code, redundant operations | Go and JVM unreachable checks, self-comparison and redundant-rethrow checks, plus narrow Rust correctness rules. | Local CFG reachability and overwritten-value analysis can grow from current support. External callers and dynamic dispatch constrain whole-project dead-code detection. |
| Magic values, mutable globals | JVM magic-number rules, Java public mutable static field, Go and Rust tests mutating process state. | Match explicit writes to shared mutable state where a concurrency or test boundary is known. Generated constants, immutable configuration, and synchronized initialization are negatives. |
| Leaky or misplaced abstraction | Configured layer dependencies surface some misplaced access. | A policy can forbid persistence types in public domain APIs or transport types in business logic. General meaning and ownership need project context. |

## Architecture and change

| Antipattern | Autoreview coverage | Feasible rule or evidence needed |
|---|---|---|
| Cyclic dependencies | `import-cycle` covers Go and JVM packages. `web-import-cycle` covers supported static relative module edges. | Resolve declared TypeScript paths and workspace package exports. Test type-only imports, re-exports, multiple configs, and dynamic boundaries explicitly. |
| Layer violations, dependency inversion violations | `layer-violation` and `transitive-layer-violation` use configured layers. Rust supports direct Cargo workspace dependencies. | Enforce additional boundary types through explicit policy, such as public-only packages or forbidden framework types in a core module. |
| Shotgun surgery, divergent change | No dedicated history check. | Repeated co-change is measurable with history. Exclude bulk renames, generated updates, and formatting commits. Direction of responsibility still needs review. |
| Hub dependency, excessive coupling, unstable dependencies | The import graph tracks edges. No general coupling finding. | A configurable fan-out budget is feasible over resolved import graphs. Fan-in and instability need complete graph coverage. Barrels and composition roots are expected exceptions. |
| Service locator, ambient dependencies, shared mutable state | JVM field injection and Java public mutable static fields. | Require known container APIs and application-layer context for a service-locator warning. DI composition roots should pass. |
| Distributed monolith, shared database, chatty services | Local loop rules can catch some repeated queries. | Assess repeated cross-service calls with resolved client identities and traces. Shared-table ownership requires deployment or schema ownership metadata. |

## Correctness and concurrency

| Antipattern | Autoreview coverage | Feasible rule or evidence needed |
|---|---|---|
| Swallowed failure, ignored error, cleanup masking an exception | Empty catches, ignored API return values, JVM return or throw in finally, Rust ignored process status. | Track known error-bearing results through assignments, branches, and return. Model deliberate error handling and propagation as negatives. |
| Null, index, panic, and arithmetic boundaries | Kotlin and TypeScript non-null assertions, Rust indexing and panic checks, JVM decimal literal precision. | Extend local guard and range tracking. Prefer a witness that reaches the risky operation, including cases involving early returns and short-circuit evaluation. |
| Cancellation leak, detached task | `go-context-cancel-on-all-paths`, Kotlin cancellation and scope checks, `rust-timeout-detaches-task`. | Track Rust task handles and structured task APIs through branches. Test deliberate ownership transfer and supported task-set semantics. |
| Resource leaks, unreleased locks | JVM resource and lock rules, Go response/rows cleanup, Rust forgotten guards and temporary owners. | Extend existing obligation tracking to well-identified acquisition/release pairs, including exceptions, all-path cleanup, and ownership transfer. |
| Races, deadlocks, non-atomic updates | Java map check-then-act and static non-thread-safe date fields. | Track lock acquisition order by identity. Shared state plus different call-site orders can justify an advisory; universal deadlock or race detection needs runtime or stronger verification. |
| Blocking async, sequential work, discarded promises, synchronous thread launch | Kotlin coroutine and JVM Reactor checks, Rust blocking-in-async checks, Java `java-thread-run-directly`, web `*-discarded-promise-combinator`. | The web rule checks standalone built-in combinator calls. General floating calls need type information. Parallelizing sequential awaits needs proof that operations are independent. |
| Wall-clock timing, mixed units, precision loss | `rust-wall-clock-elapsed-unwrap` and JVM decimal-literal-to-BigDecimal rules. | Match explicit units passed to APIs with known incompatible units. Use monotonic-time requirements only at elapsed-time call sites. |
| Equality and hash contract violations | JVM equals-without-hashCode and equality misuse checks. | Match equality and hashing over different stable fields when both methods are simple. More complex contracts benefit from property tests. |
| Unsafe retry, duplicate effects, ordering and lost updates | `java-retry-max-attempts-unbounded` catches a direct Resilience4j builder call with an effectively unbounded attempt cap. | Extend builder and framework config resolution. Business safety needs write/idempotency-key semantics and transactional concurrency tests. |

## Performance, persistence, and observability

| Antipattern | Autoreview coverage | Feasible rule or evidence needed |
|---|---|---|
| N+1 queries, chatty I/O | Go and JVM query-in-loop checks, `java-hibernate-implicit-n-plus-one`, and `rust-sqlx-query-per-row`. | Resolve more ORM relationship and fetch-plan shapes, then validate implicit queries with query-count integration fixtures. |
| Eager fetching, unbounded reads, missing pagination | JVM eager JPA association rules. | Require a known repository API, a request-path call site, and absence of a limiting operation. Reference data and internal batch jobs need explicit exemptions or separate policy. |
| Batching defeated, transaction proxy bypass, escaping a transaction | JVM identity-generation and private AOP patterns, `java-spring-transaction-self-invocation`, `rust-sqlx-pool-write-during-transaction`. | Model framework transaction boundaries and aliases. SQL batching configuration, proxy mode, and intentional independent transactions are relevant inputs. |
| Client, allocation, or regex churn | Client construction, object construction, regex compilation, string concatenation and copy patterns across supported languages. | Broaden loop-invariance checks only for APIs with known reuse safety. Per-iteration request objects and thread-confined mutable builders are negatives. |
| Quadratic search, repeated serialization, boxing | Nested-loop linear search, loop-invariant serialization, Kotlin boxed-array rules, web JSON clone pattern. | Establish collection bounds and invariance through types and flow. Small fixed-size loops and input-dependent serialization should pass. |
| Missing indexes, ineffective caching, stale cache | No dedicated rule. | A known query and available schema can identify some index mismatches. Query plans settle actual index use. Cache correctness needs keys, invalidation policy, and write ordering. |
| Retry storm, unbounded fan-out, missing backpressure | Direct Resilience4j maximum-attempt misuse. Rust timeout and channel checks cover separate lifecycle and argument issues. | Match input-dependent task spawning and account for known semaphores, bounded channels, and concurrency limiters before warning. Workload and policy supply context for queue pressure, noisy neighbors, and retry amplification. |
| Metrics cardinality, aggregation and unit misuse | Prometheus, Micrometer, and prom-client high-cardinality label/tag checks. | Add known unit/name mismatches and counter operations that violate monotonicity. Histograms and actual cardinality also depend on configuration and observed labels. |
| Trace span/scope leaks, missing propagation | `go-otel-span-not-ended`, `java-otel-discarded-current-scope`, and `rust-tracing-guard-across-await`. | Extend lifecycle checks to other supported API owners and prove context forwarding at known client or task boundaries. |

## Tests and security

| Antipattern | Autoreview coverage | Feasible rule or evidence needed |
|---|---|---|
| Missing, tautological, or detached assertions | Go and JVM missing assertions, tautological assertions in all six rule packs, Rust discarded match and detached test assertion checks. | Follow assertion helpers and async joins. Effective coverage of important failure modes needs behavioral evidence such as mutation tests. |
| Invalid, incomplete, or overused mocks | Mockito mixed matchers and discarded verification in Java/Kotlin; Rust Mockall expectation order and static context lifetime checks. | Detect concrete API mistakes when the mocking library is known. Mock density is advisory and needs test intent to distinguish legitimate protocol verification from brittle implementation coupling. |
| Flakes from sleeps, shared state, time, or ordering | Go parallel test state, Rust current-directory mutation, Kotlin coroutine test APIs. | Flag sleep-based synchronization only when an async operation is launched and a fixed wait stands in for a deterministic completion signal. Repetition and randomized order expose remaining flakes. |
| Snapshot abuse, missing edge coverage, low mutation score | No dedicated rule. | Check empty or unchecked snapshots at known APIs. Snapshot quality and edge coverage require assertion semantics, domain cases, and test execution. |
| SQL, command, path, redirect, SSRF, template and log injection | Known source/sink and syntax rules cover selected frameworks, with Rust SQL API-specific checks. | Extend source/sink coverage conservatively and test safe construction, parameter binding, and framework-specific validation paths. |
| Embedded credentials, weak crypto, insecure TLS, permissive CORS, unverified JWT | Language-specific rules for these APIs. Coverage varies by pack. | Add precise API/config variants with secure negative fixtures. Effective deployment security depends on config precedence and runtime policy. |
| Missing authorization, cross-tenant access | No general authorization rule. | Use declared route, ownership, and authorization policies. Check whether typed route/data access reaches an approved guard on all applicable paths. |
| Sensitive data in telemetry | Web `*-credential-env-in-console` catches direct console arguments that read a credential-named environment variable. Log injection is a separate finding. | Add known logging sinks, trace attributes, and alias flow with explicit redaction negatives. Arbitrary personal data needs domain classification. |

## References

- [PMD Java design rules](https://docs.pmd-code.org/pmd-doc-7.29.0/pmd_rules_java_design.html) document concrete design metrics.
- [PMD constructor-dispatch rule](https://docs.pmd-code.org/pmd-doc-7.22.0/pmd_rules_java_errorprone.html#constructorcallsoverridablemethod) describes calls to overridable methods during initialization.
- [Java Thread API](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/lang/Thread.html#start()) describes starting a separate thread.
- [typescript-eslint floating-promise rule](https://typescript-eslint.io/rules/no-floating-promises/) describes the type-aware version of promise handling.
- [Mockito verification API](https://site.mockito.org/javadoc/current/org/mockito/Mockito.html#verify-T-) describes verification of a method call.
- [Resilience4j retry configuration](https://resilience4j.readme.io/docs/retry) defines the attempt budget and wait options.
- [Microsoft Azure Retry Storm antipattern](https://learn.microsoft.com/en-us/azure/architecture/antipatterns/retry-storm/) describes retries amplifying downstream load.
- [OWASP Logging Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html#data-to-exclude) identifies sensitive data to keep out of logs.
