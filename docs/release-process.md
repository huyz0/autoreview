# Release process

The release workflow builds Linux x86-64 musl and macOS Intel and Apple
Silicon archives, checks the binaries, and publishes checksums and a Homebrew
formula. Windows can be built from source but does not yet have a packaged,
signed release artifact. The workflow smoke-runs the native Linux and Apple
Silicon binaries; the cross-built Intel macOS binary receives an architecture
check and still needs an Intel runtime check before release.

Before creating a tag, update the workspace version and lockfile and verify
that the intended commit has passed review and CI. The tag must be `v`
followed by the workspace version. Exercise installation, `doctor`, a diff
review, replay, and SARIF on each supported platform; record any platform that
could not be tested. Verify the archives against `checksums.txt` and the
attached formula against those same hashes.

A tag with a SemVer prerelease suffix, such as `v0.2.0-rc.1`, is published as
a GitHub prerelease and explicitly not marked latest. It does not update the
stable Homebrew tap. To install a candidate with the script, set
`AUTOREVIEW_VERSION` to the candidate tag. Stable tags use the normal release
and tap flow. Checksums detect accidental corruption; the current workflow
does not provide a signed provenance or an independently signed checksum.
