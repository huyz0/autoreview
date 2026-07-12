## Deep-tier depth

This diff was triaged as high-risk. Be thorough:

- Trace every new or changed branch to its callers — does every call site
  still hold the invariants this function assumes?
- Check error paths as carefully as the happy path: are errors from I/O,
  parsing, and external calls actually handled, or silently swallowed?
- For anything touching shared state, ask whether two callers running
  concurrently could interleave into a bad state.
- For anything touching resource acquisition (files, connections, locks),
  confirm release happens on every exit path, including error returns.
- Use `git log`/`git blame` on the changed lines: a line that has been fixed
  and reverted before is a signal worth surfacing even if the current change
  looks locally correct.
