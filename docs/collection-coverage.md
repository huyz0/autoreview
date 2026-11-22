# Collection performance and retention

Autoreview checks two common sources of collection cost. A pipeline can build
an intermediate collection even though its consumer can finish early. A small
result or expired entry can also keep a much larger allocation or object
graph reachable. Diagnose allocation rate and retained memory separately.
Allocation profiling shows how much work the pipeline does. A heap profile
can show what still owns an old allocation.

## Rules by language

| Language | Performance | Retention and correctness |
|---|---|---|
| Go | `go-collected-map-key-count` detects counting keys after collecting them into a slice. The existing nested-loop rule detects selected repeated linear searches. | `go-small-slice-retains-large-buffer` checks an adjacent allocation and returned view. `go-discarded-slice-delete-result` catches ignored results from selected shortening operations. The existing backing-array rule detects certain aliasing problems during append. |
| Java | `java-stream-materialized-before-short-circuit` and `java-stream-sorted-before-first` find list round trips and full sorts immediately before early-result operations. | `java-weak-map-value-retains-key` checks key-as-value writes to a typed `WeakHashMap` parameter. The existing check-then-act rule covers selected non-atomic map operations. |
| Kotlin | `kotlin-sequence-materialized-before-short-circuit` catches a sequence-to-list conversion before an early-result operation. `kotlin-sorted-before-first` checks full natural-order sorts followed by `firstOrNull`. The existing eager-chain rule covers adjacent `map` and `filter` operations. | `kotlin-weak-map-value-retains-key` checks direct writes to a JVM `WeakHashMap` parameter. |
| JavaScript, TypeScript, TSX | The `filter-for-existence` rules detect temporary arrays created for a boolean test. TypeScript rules also run on TSX. | The `small-buffer-view-escapes` rules detect a short Node Buffer view returned immediately after an explicit large allocation. The `inline-listener-removal` rules detect cleanup calls that supply a fresh function object. |
| Rust | `rust-collect-vec-before-next` detects a `Vec` built only to read its first item. Clippy also supplies collection lints where Cargo-based analysis runs. | `rust-forgotten-vec-drain` checks explicit calls to forget a drain, and `rust-forgotten-local-vec-drain` resolves the method-call form on a typed local or parameter. `rust-collection-strong-self-cycle` detects a strong clone of the collection owner stored in an element of that collection. |

## Early-result queries and repeated work

- Use a count or existence operation that works on the original input when
  the intermediate result has no other consumer. Go's `len(map)` does not
  require traversing keys. JavaScript's `some` and Kotlin's `any` can stop at
  the first match. Moving to a short-circuiting operation changes which
  callbacks run, so preserve any side effects the caller depends on.
  [Go slices.Collect](https://pkg.go.dev/slices#Collect) · [ECMAScript Array.some](https://tc39.es/ecma262/multipage/indexed-collections.html#sec-array.prototype.some) · [Kotlin predicate operations](https://kotlinlang.org/docs/collection-filtering.html#test-predicates)
- Keep stream or iterator adapters together until a consumer needs an owned
  collection. When the caller needs all results, a Kotlin sequence can avoid
  intermediate lists but can cost more for small or simple inputs. Returning
  a snapshot or iterating over an expensive result multiple times can justify
  materialization. Sorting and some stateful operations still need to inspect
  or buffer input even within a lazy pipeline.
  [Java streams](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/util/stream/package-summary.html) · [Kotlin sequences](https://kotlinlang.org/docs/sequences.html) · [Rust iterators](https://doc.rust-lang.org/book/ch13-02-iterators.html)
- Review the access pattern before switching to a set or map. Repeated scans
  over the same large input may justify an index. Inputs used once, small
  bounded collections, custom equality, and iteration-order requirements can
  make a scan the right choice. For concurrent maps, a compound update needs
  its own atomicity guarantee.
  [Java ConcurrentHashMap](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/util/concurrent/ConcurrentHashMap.html)

## Retained memory and collection ownership

- A Go subslice or a Node Buffer view can retain the source allocation after
  the producer returns. Copy at the ownership boundary when the result has a
  much longer lifetime and only needs a small region. Retaining a shared view
  can be intentional for a short-lived consumer or for shared mutation.
  The built-in checks use adjacent statements, decimal allocation literals of
  at least 100,000 bytes, and a returned prefix of at most 99 bytes. Java and
  Kotlin list views have a similar backing-collection relationship. A future
  list-view rule needs evidence of the source size and the view's lifetime.
  [Go slices internals](https://go.dev/blog/slices-intro) · [Node Buffer views](https://nodejs.org/api/buffer.html#bufsubarraystart-end) · [Kotlin list views](https://kotlinlang.org/docs/list-operations.html#retrieve-list-parts)
- In Go, update the slice header after a shortening operation. From Go 1.22,
  selected `slices` helpers clear obsolete elements so pointers do not
  survive in the old tail. A manual reslice such as `s = s[:0]` can still
  keep pointers reachable through the reused buffer. Flagging arbitrary
  reslices needs the element type and the buffer's lifetime. Clearing a
  buffer that intentionally shares storage can change another caller's view.
  [Go slice deletion and liveness](https://go.dev/blog/generic-slice-functions)
- A JVM `WeakHashMap` holds values strongly. A value that owns its own key
  can defeat weak-key eviction. Rust `Rc` and `Arc` have a related ownership
  problem when an element owns the collection containing it. A weak
  back-reference can preserve navigation without extending the inner
  object's lifetime. Use independent values or explicit lifetime management
  when ownership is required.
  [Java WeakHashMap](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/util/WeakHashMap.html) · [Rust reference cycles](https://doc.rust-lang.org/stable/book/ch15-06-reference-cycles.html)
- Listener registration and removal depend on identity. A fresh inline
  function cannot remove the previously registered handler. Pending-request
  maps have a similar requirement for the original collection and key.
  A scoped call-sequence rule can use `identity` to track the collection and
  key separately at branch joins and exits. It follows direct aliases
  and reassignments through eligible local imports and loops. It tracks helper return values. It checks
  cleanup through catch handlers, finalizers, and conditional Go defers.
  Use it where the caller owns cleanup for the registered entry. Caches, transferred ownership,
  expiration, and append-only storage have different lifetime contracts.
  [Keyed call-sequence DSL](../.claude/skills/authoring-rules/reference/rule-dsl.md#track-a-collection-entry) · [DOM listener removal](https://dom.spec.whatwg.org/#dom-eventtarget-removeeventlistener)

## Cache limits and expiration

An entry count or weight limit controls cache growth during a burst. An
expiration policy controls how long individual entries remain useful.
Cleanup scheduling determines when expired entries leave memory. A cache
with fixed keys or explicit ownership may intentionally keep each entry for
its full lifetime.

| Cache API | Rule coverage | Relevant configuration |
|---|---|---|
| Go `patrickmn/go-cache` | `go-cache-default-no-expiration` detects a constructor with a non-positive literal or a named no-expiration default. Individual writes can still supply a TTL. | Set a default TTL and cleanup interval when entries should expire and be reclaimed without explicit deletion. [go-cache constructor](https://pkg.go.dev/github.com/patrickmn/go-cache#New) |
| Java and Kotlin Caffeine | The `caffeine-default-eviction` rules cover direct `newBuilder().build()` and supported variants without an eviction policy. | Choose size, weight, expiration, or reference-based eviction according to the workload. [Caffeine eviction](https://github.com/ben-manes/caffeine/wiki/Eviction) |
| JavaScript, TypeScript, TSX `lru-cache` | The `lru-cache-ttl-without-purge` rules detect an inline TTL with no size bound or explicit pruning. Dynamic options need configuration analysis. | Set `max` or `maxSize`, or enable `ttlAutopurge` when a TTL-only cache is intentional. TTL-only pruning does not set a maximum size during bursts. [lru-cache documentation](https://isaacs.github.io/node-lru-cache/) |
| Rust Moka | `rust-moka-default-eviction` checks direct default builders for the synchronous and asynchronous caches. | Use `max_capacity` and a weigher where values have unequal cost. Add expiration when entries need it. [Moka cache documentation](https://docs.rs/moka/latest/moka/sync/struct.Cache.html) |

For a hot-path change, measure elapsed time and allocated bytes using realistic
input sizes, predicate selectivity, and terminal operations. For an apparent
leak, inspect retained objects after the operation that should release them,
such as request completion, owner disposal, or cache expiry.
