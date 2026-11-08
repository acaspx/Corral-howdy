#!/usr/bin/env bash
# Scope guard for PRs (docs/06-loop-engineering.md §3, rule 4; AGENTS.md rule 1).
#
# Fails when a PR changes or deletes EXISTING test code without a "## Test changes" section in the
# PR body that says something other than "none". Adding tests is always fine: writing tests first
# is the point. Existing test code means:
#   - any line of a test file (tests/ folders, *.test.ts(x), *Tests.swift, eval scenarios), and
#   - any line of a Rust file at or below its first #[cfg(test)]. clippy's items_after_test_module
#     lint (on via -D warnings) keeps test modules at the end of the file, so this covers them.
#
# Usage: scope_guard.sh <base-ref>     (PR body comes from $PR_BODY)
# CI runs the copy of this script from the BASE commit, so a PR can't weaken its own guard.
set -euo pipefail

base="${1:?usage: scope_guard.sh <base-ref>}"
body="${PR_BODY:-}"

is_test_file() {
  case "$1" in
    */tests/*|tests/*|*_test.rs|*.test.ts|*.test.tsx|*.spec.ts|*.spec.tsx|*Tests.swift|*/Tests/*) return 0 ;;
    evals/scenarios/*.yaml|evals/scenarios/*.yml|evals/safety/*) return 0 ;;
    *) return 1 ;;
  esac
}

# Line number of the first #[cfg(test)] in the base version of a Rust file (empty if none).
test_module_line() {
  case "$1" in
    *.rs) git show "$base:$1" 2>/dev/null | grep -n -m1 '#\[cfg(test)\]' | cut -d: -f1 || true ;;
    *) ;;
  esac
}

# Prints 1 if this PR changes or deletes any base-side line numbered >= $2 in file $1, else 0.
# Reads the whole diff (no early exit) so pipefail can't trip on SIGPIPE.
touches_lines_from() {
  git diff --no-renames -U0 "$base"...HEAD -- "$1" | awk -v from="$2" '
    /^@@/ {
      split($2, old, ",")             # $2 looks like -start[,count]
      start = substr(old[1], 2) + 0
      count = (length(old) > 1) ? old[2] + 0 : 1
      if (count > 0 && start + count - 1 >= from) hit = 1
    }
    END { print hit + 0 }'
}

changed=()
while IFS=$'\t' read -r status path; do
  module_line="$(test_module_line "$path")"
  case "$status" in
    D)
      if is_test_file "$path" || [ -n "$module_line" ]; then changed+=("$path (deleted)"); fi ;;
    M)
      if is_test_file "$path"; then
        [ "$(touches_lines_from "$path" 1)" = 1 ] && changed+=("$path")
      elif [ -n "$module_line" ] && [ "$(touches_lines_from "$path" "$module_line")" = 1 ]; then
        changed+=("$path (inline test module)")
      fi ;;
    *) ;;   # A = added, always fine
  esac
done < <(git diff --no-renames --name-status "$base"...HEAD)

if [ "${#changed[@]}" -eq 0 ]; then
  echo "scope-guard: no existing test code changed."
  exit 0
fi

# Extract the "## Test changes" section of the PR body.
section="$(printf '%s\n' "$body" | awk '
  /^##[[:space:]]+Test changes/ { on=1; next }
  /^##[[:space:]]/ && on { exit }
  on { print }
' | sed -e 's/<!--.*-->//g' | tr -d '[:space:]')"

if [ -z "$section" ] || [ "$(printf '%s' "$section" | tr '[:upper:]' '[:lower:]')" = "none" ]; then
  echo "scope-guard: existing test code changed, but the PR's \"Test changes\" section is empty or \"none\":"
  printf '  - %s\n' "${changed[@]}"
  echo "Explain each change under \"## Test changes\" in the PR description. A human must approve it."
  exit 1
fi

echo "scope-guard: existing test code changed and explained in \"Test changes\" (human approval required):"
printf '  - %s\n' "${changed[@]}"
