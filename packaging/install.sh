#!/bin/sh
# Install autoreview from a GitHub Release.
#
#   curl -fsSL https://raw.githubusercontent.com/huyz0/autoreview/main/packaging/install.sh | sh
#
# Environment:
#   AUTOREVIEW_VERSION  tag to install (default: latest release)
#   AUTOREVIEW_BIN_DIR  install directory (default: ~/.local/bin)
#
# POSIX sh, not bash: this is piped into whatever /bin/sh happens to be.
set -eu

REPO="huyz0/autoreview"
BIN_DIR="${AUTOREVIEW_BIN_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required but not installed"; }

# Resolve the download target from uname. Kept explicit rather than
# clever: an unrecognised platform must fail with a message naming what
# was detected, not silently fetch something that cannot run.
detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Linux)
      case "$arch" in
        x86_64|amd64) echo "x86_64-unknown-linux-musl" ;;
        *) die "unsupported Linux architecture: $arch (only x86_64 is published; build from source instead)" ;;
      esac
      ;;
    Darwin)
      case "$arch" in
        arm64|aarch64) echo "aarch64-apple-darwin" ;;
        x86_64) echo "x86_64-apple-darwin" ;;
        *) die "unsupported macOS architecture: $arch" ;;
      esac
      ;;
    *)
      die "unsupported OS: $os (Linux and macOS are published; build from source instead)"
      ;;
  esac
}

# The tag of the most recent release, read from the redirect GitHub issues
# for /releases/latest. Avoids the API, which rate-limits unauthenticated
# callers aggressively enough to break a plain `curl | sh`.
latest_version() {
  curl -fsSLI -o /dev/null -w '%{url_effective}' \
    "https://github.com/${REPO}/releases/latest" \
    | sed 's#.*/tag/##'
}

verify_checksum() {
  archive="$1"
  checksums="$2"
  expected="$(awk -v f="$(basename "$archive")" '$2 == f || $2 == "*"f {print $1}' "$checksums")"
  [ -n "$expected" ] || die "no checksum published for $(basename "$archive")"

  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$archive" | cut -d' ' -f1)"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$archive" | cut -d' ' -f1)"
  else
    die "need sha256sum or shasum to verify the download"
  fi

  [ "$actual" = "$expected" ] || die "checksum mismatch for $(basename "$archive") (expected $expected, got $actual)"
  say "  checksum ok"
}

main() {
  need curl
  need tar

  target="$(detect_target)"
  version="${AUTOREVIEW_VERSION:-$(latest_version)}"
  case "$version" in
    v*) ;;
    "") die "could not determine the latest release — set AUTOREVIEW_VERSION to a tag like v0.1.0" ;;
    *) version="v${version}" ;;
  esac

  name="autoreview-${version}-${target}"
  base="https://github.com/${REPO}/releases/download/${version}"

  say "installing autoreview ${version} (${target})"

  tmp="$(mktemp -d)"
  # Clean up on any exit path, including the die() failures above.
  trap 'rm -rf "$tmp"' EXIT INT TERM

  curl -fsSL "${base}/${name}.tar.gz" -o "${tmp}/${name}.tar.gz" \
    || die "download failed — does ${version} publish a ${target} build? See https://github.com/${REPO}/releases"
  curl -fsSL "${base}/checksums.txt" -o "${tmp}/checksums.txt" \
    || die "could not download checksums.txt for ${version}"

  verify_checksum "${tmp}/${name}.tar.gz" "${tmp}/checksums.txt"

  tar -xzf "${tmp}/${name}.tar.gz" -C "$tmp"
  [ -f "${tmp}/${name}/autoreview" ] || die "archive did not contain the expected autoreview binary"

  mkdir -p "$BIN_DIR"
  install -m 0755 "${tmp}/${name}/autoreview" "${BIN_DIR}/autoreview" 2>/dev/null \
    || { cp "${tmp}/${name}/autoreview" "${BIN_DIR}/autoreview" && chmod 0755 "${BIN_DIR}/autoreview"; }

  say "installed ${BIN_DIR}/autoreview"

  case ":${PATH}:" in
    *":${BIN_DIR}:"*) say "run 'autoreview doctor' to check which analyzers are available" ;;
    *)
      say ""
      say "${BIN_DIR} is not on your PATH. Add it:"
      say "  export PATH=\"${BIN_DIR}:\$PATH\""
      ;;
  esac
}

main "$@"
