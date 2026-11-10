# shellcheck shell=bash
# Shared setup for the packaging scripts. Source it: `. "$(dirname "$0")/../env.sh"`.
#
# Exports:
#   ROOT                    workspace root
#   VERSION                 [package] version from the root Cargo.toml (override: LOCALPLAYER_VERSION)
#   DIST                    output directory for release artifacts (default: $ROOT/dist/release)
#   LOCALPLAYER_BUILD_SHA   git commit baked into the binaries
#   LOCALPLAYER_BUILD_DATE  UTC build date, YYYY-MM-DD
#   CARGO_TARGET_DIR        cargo's target dir (default: $ROOT/target)

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export ROOT

# The version lives in the root [package] of Cargo.toml (same read as release.yml; awk avoids
# compiling xtask here).
package_version() {
  awk '
    /^\[/ { in_pkg = ($0 == "[package]") ; next }
    in_pkg && $1 == "version" { gsub(/[" ]/, "", $3); print $3; exit }
  ' "$ROOT/Cargo.toml"
  return $?
}

VERSION="${LOCALPLAYER_VERSION:-$(package_version)}"
if [[ -z "$VERSION" ]]; then
  echo "error: could not read [package] version from $ROOT/Cargo.toml" >&2
  exit 1
fi
export VERSION

DIST="${DIST:-$ROOT/dist/release}"
mkdir -p "$DIST"
export DIST

if [[ -z "${LOCALPLAYER_BUILD_SHA:-}" ]]; then
  LOCALPLAYER_BUILD_SHA="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
fi
export LOCALPLAYER_BUILD_SHA
export LOCALPLAYER_BUILD_DATE="${LOCALPLAYER_BUILD_DATE:-$(date -u +%Y-%m-%d)}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

# Emit a GitHub Actions warning (plain stderr outside Actions).
warn() {
  if [[ -n "${GITHUB_ACTIONS:-}" ]]; then echo "::warning::$*" >&2; else echo "warning: $*" >&2; fi
  return 0
}

# Copy licence and readme files that exist into a package directory.
copy_docs() {
  local dest="$1" f
  for f in README.md LICENSE LICENSE-MIT LICENSE-APACHE COPYRIGHT NOTICE ATTRIBUTION.md; do
    if [[ -f "$ROOT/$f" ]]; then cp "$ROOT/$f" "$dest/"; fi
  done
  return 0
}

# Portable SHA-256 of a file (prints just the hash).
sha256() {
  local file="$1"
  if command -v sha256sum >/dev/null; then sha256sum "$file" | cut -d' ' -f1; else shasum -a 256 "$file" | cut -d' ' -f1; fi
  return $?
}
