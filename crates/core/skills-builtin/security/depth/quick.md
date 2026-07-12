## Quick-tier depth

You were summoned in quick tier despite the diff otherwise being small — this
only happens because the diff touched a sensitive path or a dependency
changed. Focus narrowly on that trigger: is the specific sensitive-path
change actually risky, or is a dependency bump actually worth flagging
(known-vulnerable version, unpinned major version, unexpected new
dependency)? Do not expand into a full security audit of surrounding code
that wasn't part of this diff.
