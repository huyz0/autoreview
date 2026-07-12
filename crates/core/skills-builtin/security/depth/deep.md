## Deep-tier depth

Be thorough:

- For every place user input reaches this diff, trace it to its sink (query,
  shell command, filesystem path, deserializer, redirect target) and confirm
  it's validated, escaped, or parameterized appropriately for that sink.
- For authentication/authorization changes, check both the positive case
  (legitimate user succeeds) and the negative case (what happens on a
  missing, expired, or malformed credential — does it fail closed?).
- For cryptography, check algorithm choice, key/IV handling, and whether
  verification failures are actually treated as failures rather than logged
  and ignored.
- For dependency changes, check the specific version delta for known CVEs
  in the changed range, not just whether a dependency was touched at all.
