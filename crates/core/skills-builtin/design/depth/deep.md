## Deep-tier depth

Be thorough:

- Grep for other implementations of the same responsibility elsewhere in the
  codebase; if this diff introduces a second, divergent way of doing
  something already solved elsewhere, that's worth flagging even if the new
  code works correctly on its own.
- Check whether this diff's new public surface (exported functions, new
  parameters, new config keys) is the minimal surface needed, or whether it
  leaks internal implementation detail that will be hard to change later.
- Check whether this diff duplicates logic that could be shared with a
  nearby existing abstraction, versus genuinely needing its own.
- If this diff changes an existing interface's behavior without changing its
  signature, treat that as a design concern worth surfacing even though no
  compiler would catch it.
