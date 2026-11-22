# Loop and recursion rules

Autoreview checks specific control-flow and collection patterns in Go, Java,
Kotlin, JavaScript, TypeScript, TSX, and Rust. JSX uses the JavaScript grammar.
TypeScript rules also run against TSX. The checks identify local syntax and
selected receiver types. They can point to work that repeats, even when the
cost, termination, or effect on callers needs runtime evidence.

## Coverage by stack

| Stack | Control flow and correctness | Repeated work |
| --- | --- | --- |
| Go | Detects a counter moving away from its bound and a zero-based loop that indexes a slice with an inclusive length bound. Detects a bare loop with a sole `select` and empty `default`. Existing checks cover capture on older Go versions, defers that outlive their iteration, and cancellation. | Detects `slices.Contains` on an identifier with the current range value. Existing checks cover selected nested linear searches, explicit queries, regular expression compilation, and HTTP client construction in loops. |
| Java | Detects the same counter direction problem and inclusive array length with indexing. | Detects `contains` on a fully qualified `java.util.ArrayList` parameter with the enhanced-for item. Existing rules cover explicit query-in-loop, selected Hibernate N+1, nested linear search, and some eager stream work. |
| Kotlin | Detects an inclusive `0..items.size` range that indexes the same collection using the loop variable. | Detects `contains` on a `List` parameter with the current for-loop item. Existing checks cover spread copying, repeated queries, nested linear search, eager sequence materialization, and blocking `Thread.sleep` in suspend functions. |
| JavaScript, TypeScript, TSX | Detects wrong counter direction, inclusive length plus indexed access, inline async callbacks passed to `forEach`, and supported named React Hooks imported from React and called in a loop. React's `use` API is allowed in loops. | Detects `includes` with a current `for…of` item, strict field equality between two nested `for…of` bindings, and repeated `shift()` from the array driving a length-based `while` loop. |
| Rust | Detects indexing with the induction variable from `0..=items.len()`. | Detects repeated membership scans on shared slice parameters, field-equality joins between nested loop bindings, and `Vec::remove(0)` from a typed parameter in a drain loop. Existing checks cover selected per-row SQLx calls, Reqwest client creation, and eager iterator materialization. |

Every stack in the table checks direct self-recursion in selected single-call
bodies and a two-branch recurrence with a Fibonacci-shaped base case and
return expression. The direct rules match selected zero-argument and
single-argument declarations. The recurrence rules are performance hints.

The [loop rule families](../crates/core/rule-families/loops.yml) store shared
metadata and matchers once. Each language variant can override a field.
Cargo expands the families into the built-in catalog and retains each
language's rule ID. A fixture's `cases.yml` can reuse JavaScript sources
for TypeScript or supply one source template with positive and negative
replacements.

## Control flow and termination

- In a counter loop, compare the same variable that advances. An increment
  with a lower bound or a decrement with an upper bound can keep the
  condition true. A body may still exit by breaking, returning, or changing
  another value. The direction checks look for direct `++` and `--` updates.
  For a zero-based index, the last valid index is length minus one. A guard
  before an access can make an inclusive traversal intentional. The index
  rules match selected zero-based loops and the same collection and index in
  the body. Iterator-based loops avoid maintaining an index when it is unused.
  [Go for statements](https://go.dev/ref/spec#For_statements) ·
  [Java for statements](https://docs.oracle.com/javase/specs/jls/se21/html/jls-14.html#jls-14.14.1) ·
  [ESLint for-direction](https://eslint.org/docs/latest/rules/for-direction) ·
  [Kotlin ranges](https://kotlinlang.org/docs/ranges.html) ·
  [Rust loop expressions](https://doc.rust-lang.org/reference/expressions/loop-expr.html)
- An infinite worker loop can be correct when it blocks for work and has a
  shutdown path. An empty Go `select` default immediately retries when no
  channel is ready. Rust supports bounded optimistic spinning before blocking.
  Kotlin coroutine cancellation is cooperative and a long CPU loop without
  suspension can use `ensureActive` or check `isActive`. General busy-wait and
  cancellation rules need to follow the work path and distinguish blocking,
  yielding, and checks hidden in helpers.
  [Go select statements](https://go.dev/ref/spec#Select_statements) ·
  [Go pipelines and cancellation](https://go.dev/blog/pipelines) ·
  [Rust spin_loop](https://doc.rust-lang.org/std/hint/fn.spin_loop.html) ·
  [Kotlin coroutine cancellation](https://kotlinlang.org/api/kotlinx.coroutines/kotlinx-coroutines-core/kotlinx.coroutines/is-active.html)
- Recursion needs a stopping condition and progress toward it on each path.
  For a user-sized tree or graph, check maximum depth and whether nodes can
  form cycles. A stack-based traversal can preserve DFS order if it pushes
  children in reverse visit order. A visited set works when node identity
  captures the relevant state. The direct checks cover single-call bodies
  without a stopping branch. Kotlin can lower eligible `tailrec` functions to
  loops. Rust also checks unconditional recursion when compiler analysis is
  enabled. Mutual recursion, recursive getters, and recursive calls hidden
  behind dispatch need call resolution and a path analysis before reporting a
  missing exit. Test zero input, depth limits, and deliberate cycles.
  [Java StackOverflowError](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/lang/StackOverflowError.html) ·
  [Kotlin tail recursion](https://kotlinlang.org/docs/functions.html#tail-recursive-functions) ·
  [Rust unconditional_recursion](https://doc.rust-lang.org/rustc/lints/listing/warn-by-default.html#unconditional-recursion)

## Repeated work and hot paths

| Pattern | When a rule has enough evidence | What changes the recommendation |
| --- | --- | --- |
| Nested joins and membership scans | Two distinct loop bindings compared by field equality, or a known scan API called with the current item. A list scan inside a traversal over another input can do work proportional to the product of their lengths. | An index must preserve equality, duplicate matches, and encounter order. Account for construction and memory costs. A fixed-size inner loop, shrinking search window, early exit, or true Cartesian product may be appropriate. |
| Front-removal and loop mutation | A length-driven drain repeatedly calls array `shift` or Rust `Vec::remove(0)`. | Confirm how callers observe the original collection. A head index can retain references unless consumed entries are cleared or the buffer is compacted. Use a deque for long-lived FIFO workloads. Mutation during a traversal needs the iterator's contract and the receiver's identity. |
| Overlapping recursion | An additive recurrence calls itself on both `n - 1` and `n - 2` after the base case. | Cache by the full state or compute bottom-up if calls are pure and recomputation is material. Divide-and-conquer over disjoint ranges can be efficient without memoization. |
| Repeated allocation and copying | Known expensive API, immutable inputs, and proof that the same work repeats. Reviewed examples include serialization, compiling regexes, string building, and constructing HTTP clients. | Measure execution count, allocated bytes, and allocation lifetime. Moving work can change evaluation order or reuse mutable state. Collection preallocation can waste memory if a filter keeps few elements. |
| Short-circuiting and iterator fusion | A collection is immediately consumed by an existence test or another operation that can stop early. See the [collection coverage](collection-coverage.md) for existing adapters and early-result rules. | Fewer callback executions can change side effects. Kotlin sequences add overhead for small or simple pipelines. JVM streams, Kotlin sequences, and Rust iterators can still buffer for sorting and other stateful stages. |
| N+1 I/O and batching | A call performs a known query for each row or a known lazy property access triggers it. Existing Hibernate and SQLx rules recognize selected shapes. | Confirm with query counts and trace spans. Large fetch joins can multiply result rows, and batching needs limits and failure handling. |
| Sequential awaits and async fan-out | Built-in array `forEach` discards the inline async callback's result. Rules for general sequential waits or task fan-out need dependency, cancellation, and concurrency evidence. | Ordering, retries, transaction boundaries, and rate limits can require serial processing. Use bounded concurrency when work is independent and the downstream capacity is finite. |
| Nested complexity and test loops | The existing structural metrics describe code shape. A test loop can repeat an assertion with no case label. | Runtime cost depends on bounds and work per iteration. Test diagnostics depend on whether failures identify the input and whether awaited assertions finish. Global depth thresholds and universal loop rewrites would obscure these distinctions. |

[Java ArrayList operation costs](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/util/ArrayList.html) ·
[Java Deque](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/util/Deque.html) ·
[ECMAScript Array.prototype.shift](https://tc39.es/ecma262/multipage/indexed-collections.html#sec-array.prototype.shift) ·
[ECMAScript Set and Map](https://tc39.es/ecma262/multipage/keyed-collections.html) ·
[Rust collection operation costs](https://doc.rust-lang.org/std/collections/index.html#cost-of-collection-operations) ·
[Dynamic programming and overlapping subproblems](https://www.ocw.mit.edu/courses/6-006-introduction-to-algorithms-fall-2011/resources/lecture-19-dynamic-programming-i-fibonacci-shortest-paths/)

[Go allocation optimizations](https://go.dev/blog/allocation-optimizations) ·
[Kotlin sequences](https://kotlinlang.org/docs/sequences.html) ·
[Java stream operations](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/util/stream/Stream.html) ·
[Hibernate fetch joins](https://docs.hibernate.org/stable/core/userguide/html_single/#hql-explicit-fetch-join) ·
[ECMAScript Array.prototype.forEach](https://tc39.es/ecma262/multipage/indexed-collections.html#sec-array.prototype.foreach) ·
[ESLint await-in-loop exceptions](https://eslint.org/docs/latest/rules/no-await-in-loop#when-not-to-use-it) ·
[React Hook ordering](https://react.dev/reference/eslint-plugin-react-hooks/lints/rules-of-hooks)

For a proposed optimization, profile first and measure with production-sized
inputs and selectivity. Compare time, allocation, and peak retained memory.
For task loops, check in-flight work and cancellation latency. For I/O, count
actual round trips. Add output equivalence cases for duplicates, ordering,
partial failure, and early termination. [Go diagnostics](https://go.dev/doc/diagnostics)

Python and SQL files lack a standalone parser in the current Autoreview rule
engine. Python's `list.pop(0)` and PostgreSQL recursive CTEs have related
patterns. A Python rule would need list identity. A PostgreSQL rule needs
query structure and, for graph cycles, knowledge of the key that identifies
a visited row. The host-language query checks in the table are the current
SQL-related coverage. [Python deque](https://docs.python.org/3/library/collections.html#collections.deque) ·
[PostgreSQL recursive queries](https://www.postgresql.org/docs/18/queries-with.html#QUERIES-WITH-RECURSIVE)
