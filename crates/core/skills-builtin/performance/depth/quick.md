## Quick-tier depth

You were summoned in quick tier because the diff otherwise looked small.
Focus narrowly on the changed lines themselves: did they introduce an
obviously quadratic loop, an unpooled resource, or a blocking call in an
async/goroutine context? Do not go looking for pre-existing performance
issues in surrounding code that wasn't part of this diff.
