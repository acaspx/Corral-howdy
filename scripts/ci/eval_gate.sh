#!/usr/bin/env bash
# Runs the eval gate (docs/05-evals.md). Used by CI and `just eval gate`.
#
# Usage: eval_gate.sh [base-ref]
#   With a base ref (CI passes the PR's base commit), the baseline is read from THAT commit, so a
#   PR can't hide a regression by editing evals/reports/baselines/main.json. Without one, the
#   working tree's baseline is used (local runs).
# Exits non-zero when the gate fails. Set CORRAL_EVALS_BIN to use a prebuilt binary.
set -euo pipefail

base="${1:-}"
baseline_path="evals/reports/baselines/main.json"
report="evals/reports/gate.json"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

args=(--suite gate --report "$report")
if [ -n "$base" ]; then
  if git cat-file -e "$base:$baseline_path" 2>/dev/null; then
    git show "$base:$baseline_path" > "$tmp/baseline.json"
    args+=(--baseline "$tmp/baseline.json")
    echo "eval-gate: baseline from $base" >&2
  else
    echo "eval-gate: no baseline at $base; comparing against nothing" >&2
  fi
elif [ -f "$baseline_path" ]; then
  args+=(--baseline "$baseline_path")
fi

if [ -n "${CORRAL_EVALS_BIN:-}" ]; then
  runner=("$CORRAL_EVALS_BIN")
else
  runner=(cargo run -q -p corral-evals --)
fi

summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
# pipefail (set above) makes this pipeline fail when the runner fails, not just when tee does.
"${runner[@]}" "${args[@]}" | tee -a "$summary"
