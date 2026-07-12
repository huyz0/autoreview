## Quick-tier depth

This is a small, low-risk diff. Report at most the 3 highest-confidence
correctness issues. Do not chase speculative edge cases or theoretical
concurrency issues unless the diff itself touches concurrent code. If you
find nothing you're confident about, return an empty findings list — that is
a valid and expected result for a small diff, not a failure.
