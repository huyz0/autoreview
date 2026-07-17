## Deep-tier depth

Be thorough:

- For every loop touching a collection whose size depends on user input or
  request volume, work out its actual big-O complexity, not just whether it
  "looks fine" — an added lookup inside an existing loop is the most common
  way O(n) quietly becomes O(n^2).
- For any new external call (DB, cache, HTTP, filesystem) inside a loop or a
  function called per-item, check whether it can be batched.
- For resource acquisition (connections, file handles, locks, thread-pool
  tasks), trace every exit path — including early returns and exceptions —
  to confirm the resource is released on all of them, not just the happy path.
- For caches or in-memory collections introduced or modified in this diff,
  check whether they have a bound (size limit, TTL, eviction policy) or can
  grow without limit as usage scales.
