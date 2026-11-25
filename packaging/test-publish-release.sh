#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -f "$tmp/bin/gh" "$tmp/dist/package.tar.gz" "$tmp/dist/checksums.txt" "$tmp/dist/autoreview.rb" "$tmp/args" "$tmp/output"; rmdir "$tmp/bin" "$tmp/dist" "$tmp"' EXIT
mkdir "$tmp/bin" "$tmp/dist"
touch "$tmp/dist/package.tar.gz" "$tmp/dist/checksums.txt" "$tmp/dist/autoreview.rb"
cat > "$tmp/bin/gh" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" > "$ARGS_FILE"
exit "${GH_TEST_EXIT:-0}"
EOF
chmod +x "$tmp/bin/gh"
export PATH="$tmp/bin:$PATH" ARGS_FILE="$tmp/args" GITHUB_OUTPUT="$tmp/output"

sh "$root/packaging/publish-release.sh" v0.2.0 example/repo "$tmp/dist"
grep -qx 'prerelease=false' "$GITHUB_OUTPUT"
if grep -qx -- '--prerelease' "$ARGS_FILE"; then exit 1; fi
if grep -qx -- '--latest=false' "$ARGS_FILE"; then exit 1; fi

: > "$GITHUB_OUTPUT"
sh "$root/packaging/publish-release.sh" v0.2.0-rc.1 example/repo "$tmp/dist"
grep -qx 'prerelease=true' "$GITHUB_OUTPUT"
grep -qx -- '--prerelease' "$ARGS_FILE"
grep -qx -- '--latest=false' "$ARGS_FILE"
grep -qx -- '--verify-tag' "$ARGS_FILE"

: > "$GITHUB_OUTPUT"
sh "$root/packaging/publish-release.sh" 'v0.2.0+build-1' example/repo "$tmp/dist"
grep -qx 'prerelease=false' "$GITHUB_OUTPUT"
if grep -qx -- '--prerelease' "$ARGS_FILE"; then exit 1; fi

# A dispatch input is passed as an argument, never evaluated as shell code.
(cd "$tmp" && sh "$root/packaging/publish-release.sh" 'v0.2.0-rc.$(touch injected)' example/repo "$tmp/dist")
test ! -e "$tmp/injected"
grep -Fxq 'v0.2.0-rc.$(touch injected)' "$ARGS_FILE"

: > "$GITHUB_OUTPUT"
export GH_TEST_EXIT=1
if sh "$root/packaging/publish-release.sh" v0.2.0-rc.1 example/repo "$tmp/dist"; then exit 1; fi
test ! -s "$GITHUB_OUTPUT"
unset GH_TEST_EXIT

rm -f "$ARGS_FILE"
unset GITHUB_OUTPUT
if sh "$root/packaging/publish-release.sh" v0.2.0 example/repo "$tmp/dist" 2>/dev/null; then exit 1; fi
test ! -e "$ARGS_FILE"
