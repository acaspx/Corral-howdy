#!/usr/bin/env bash
# Scope guard for PRs (docs/06-loop-engineering.md §3, rule 4).
#
# Fails when a PR modifies or deletes EXISTING test files without a "## Test changes" section in
# the PR body that says something other than "none". New test files are always fine: writing
# tests first is the point.
#
# Usage: scope_guard.sh <base-sha>     (PR body comes from $PR_BODY)
# The per-issue allowed-paths fence (rule 3) is added in M1, once issues carry machine-readable paths.
set -euo pipefail

base="${1:?usage: scope_guard.sh <base-sha>}"
body="${PR_BODY:-}"

# Test files: Rust tests dirs and #[cfg(test)]-bearing files are hard to detect by path, so we
# track the conventional locations plus any file whose name marks it as a test.
is_test_file() {
  case "$1" in
    */tests/*|*_test.rs|*.test.ts|*.test.tsx|*.spec.ts|*.spec.tsx|*Tests.swift|*/Tests/*|evals/scenarios/*|evals/safety/*) return 0 ;;
    *) return 1 ;;
  esac
}

changed=()
while IFS=$'\t' read -r status path rest; do
  # M = modified, D = deleted, R* = renamed (path is the old name). A = added (allowed).
  case "$status" in
    M|D|R*) is_test_file "$path" && changed+=("$path") ;;
    *) ;;
  esac
done < <(git diff --name-status "$base"...HEAD)

if [ "${#changed[@]}" -eq 0 ]; then
  echo "scope-guard: no existing test files changed."
  exit 0
fi

# Extract the "## Test changes" section of the PR body.
section="$(printf '%s\n' "$body" | awk '
  /^##[[:space:]]+Test changes/ { on=1; next }
  /^##[[:space:]]/ && on { exit }
  on { print }
' | sed -e 's/<!--.*-->//g' | tr -d '[:space:]')"

if [ -z "$section" ] || [ "$(printf '%s' "$section" | tr '[:upper:]' '[:lower:]')" = "none" ]; then
  echo "scope-guard: these existing test files changed, but the PR's \"Test changes\" section is empty or \"none\":"
  printf '  - %s\n' "${changed[@]}"
  echo "Explain each change under \"## Test changes\" in the PR description. A human must approve it."
  exit 1
fi

echo "scope-guard: existing test files changed and explained in \"Test changes\" (human approval required):"
printf '  - %s\n' "${changed[@]}"
