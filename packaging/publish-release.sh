#!/bin/sh
# Publish already-built assets. RELEASE_TAG and repository are data, not shell code.
set -eu
: "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"

tag=$1
repo=$2
dist=$3
core=${tag%%+*}
case "$core" in
  *-*) prerelease=true ;;
  *) prerelease=false ;;
esac

set --
if [ "$prerelease" = true ]; then
  set -- --prerelease --latest=false
fi
gh release create "$tag" --verify-tag --repo "$repo" --title "$tag" \
  --generate-notes "$@" "$dist"/*.tar.gz "$dist/checksums.txt" "$dist/autoreview.rb"
printf 'prerelease=%s\n' "$prerelease" >> "$GITHUB_OUTPUT"
