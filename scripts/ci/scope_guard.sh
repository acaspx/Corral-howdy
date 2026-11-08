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

# The guard fails closed: if any git lookup fails, the script exits non-zero instead of
# concluding "no test code changed". Every git call is a plain assignment at the top level of the
# loop, so `set -e` sees its failure; the helpers below only parse text.

# Line number of the first #[cfg(test)] in Rust source text on stdin (empty if none).
first_test_module_line() {
  awk '/#\[cfg\(test\)\]/ && !n { n = NR } END { if (n) print n }'
}

# Prints 1 if the -U0 diff on stdin changes or deletes any base-side line numbered >= $1, else 0.
touches_lines_from() {
  awk -v from="$1" '
    /^@@/ {
      split($2, old, ",")             # $2 looks like -start[,count]
      start = substr(old[1], 2) + 0
      count = (length(old) > 1) ? old[2] + 0 : 1
      if (count > 0 && start + count - 1 >= from) hit = 1
    }
    END { print hit + 0 }'
}

if ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
  echo "scope-guard: base ref '$base' not found. Check out full history (fetch-depth: 0)." >&2
  exit 2
fi

changes="$(git diff --no-renames --name-status "$base"...HEAD)"

changed=()
while IFS=$'\t' read -r status path; do
  if [ -z "$status" ]; then continue; fi
  case "$status" in
    A) continue ;;   # new files are always fine
    M|D) ;;
    *) echo "scope-guard: unexpected diff status '$status' for $path" >&2; exit 2 ;;
  esac

  module_line=""
  if [[ "$path" == *.rs ]]; then
    base_text="$(git show "$base:$path")"
    module_line="$(printf '%s\n' "$base_text" | first_test_module_line)"
  fi

  if [ "$status" = D ]; then
    if is_test_file "$path" || [ -n "$module_line" ]; then changed+=("$path (deleted)"); fi
    continue
  fi

  diff="$(git diff --no-renames -U0 "$base"...HEAD -- "$path")"
  if is_test_file "$path"; then
    hit="$(printf '%s\n' "$diff" | touches_lines_from 1)"
    if [ "$hit" = 1 ]; then changed+=("$path"); fi
  elif [ -n "$module_line" ]; then
    hit="$(printf '%s\n' "$diff" | touches_lines_from "$module_line")"
    if [ "$hit" = 1 ]; then changed+=("$path (inline test module)"); fi
  fi
done <<< "$changes"

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
