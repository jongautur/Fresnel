#!/bin/sh
# Check that every version number agrees with the Cargo workspace version
# ([workspace.package] in Cargo.toml), the single source of truth.
#
# Usage: scripts/check-version.sh [vX.Y.Z]
#   With a tag (or refs/tags/vX.Y.Z), it must match too. Used by CI and the
#   release workflow; run it before tagging.
# Needs node (already required for the frontend build).
set -eu

cd "$(dirname "$0")/.."

fail=0
err() {
  echo "error: $*" >&2
  fail=1
}

cargo_version=$(awk '
  /^\[/ { in_section = ($0 == "[workspace.package]") }
  in_section && /^version[ \t]*=/ {
    sub(/^version[ \t]*=[ \t]*"/, ""); sub(/".*$/, ""); print; exit
  }' Cargo.toml)

if [ -z "$cargo_version" ]; then
  echo "error: no version in [workspace.package] of Cargo.toml" >&2
  exit 1
fi
echo "Cargo.toml [workspace.package]: $cargo_version"

# Every member crate inherits the workspace version.
for manifest in src-tauri/Cargo.toml crates/*/Cargo.toml; do
  [ -f "$manifest" ] || continue
  grep -Eq '^version\.workspace[ \t]*=[ \t]*true' "$manifest" ||
    err "$manifest does not use version.workspace = true"
done

json_field() { # file, JS expression on `j`
  node -e 'const j = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
const v = eval(process.argv[2]); process.stdout.write(v === undefined ? "" : String(v));' "$1" "$2"
}

check() { # label, value
  if [ "$2" != "$cargo_version" ]; then
    err "$1 is '$2', expected '$cargo_version'"
  else
    echo "$1: $2"
  fi
}

check "package.json" "$(json_field package.json 'j.version')"
check "package-lock.json" "$(json_field package-lock.json 'j.version')"
check "package-lock.json packages[\"\"]" "$(json_field package-lock.json 'j.packages[""].version')"

# Tauri falls back to the Cargo package version when tauri*.conf.json has none;
# keep it that way, but if someone adds one it must still agree.
for conf in src-tauri/tauri.conf.json src-tauri/tauri.*.conf.json; do
  [ -f "$conf" ] || continue
  v=$(json_field "$conf" 'j.version')
  [ -z "$v" ] || check "$conf" "$v"
done

if [ $# -gt 0 ]; then
  tag=${1#refs/tags/}
  case "$tag" in
    v*) check "tag $tag" "${tag#v}" ;;
    *) err "tag '$tag' does not start with 'v'" ;;
  esac
fi

if [ "$fail" -ne 0 ]; then
  echo "Version mismatch: bump [workspace.package] version in Cargo.toml, package.json" >&2
  echo "(npm version --no-git-tag-version X.Y.Z), then cargo update --workspace." >&2
  exit 1
fi
echo "Versions agree: $cargo_version"
