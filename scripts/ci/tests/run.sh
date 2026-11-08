#!/usr/bin/env bash
# Tests for the CI scripts (TP-CI-01..08, docs/04-test-plan.md). Each case builds a throwaway git
# repo, makes a "PR" commit and checks the script's verdict. Run: scripts/ci/tests/run.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
ci="$(cd "$here/.." && pwd)"
repo_root="$(cd "$ci/../.." && pwd)"
evals_bin="${CORRAL_EVALS_BIN:-$repo_root/target/debug/corral-evals}"
pass=0
fail=0

ok()  { pass=$((pass + 1)); echo "  ok   $1"; }
bad() { fail=$((fail + 1)); echo "  FAIL $1"; }

# expect <name> <0|1> <command...>: the command's exit status must be zero (0) or non-zero (1).
expect() {
  local name="$1" want="$2"; shift 2
  local got=0
  "$@" >/dev/null 2>&1 || got=1
  if [ "$got" = "$want" ]; then ok "$name"; else bad "$name (exit status $( [ "$got" = 0 ] && echo zero || echo non-zero ))"; fi
}

# A fresh repo with one base commit. Prints its path; the base commit is tagged `base`.
new_repo() {
  local dir
  dir="$(mktemp -d)"
  git -C "$dir" init -q
  git -C "$dir" config user.email ci@example.test
  git -C "$dir" config user.name ci
  mkdir -p "$dir/crates/demo/src" "$dir/crates/demo/tests"
  cat > "$dir/crates/demo/src/lib.rs" <<'RS'
pub fn answer() -> u32 {
    42
}

#[cfg(test)]
mod tests {
    #[test]
    fn answer_is_42() {
        assert_eq!(super::answer(), 42);
    }
}
RS
  printf '#[test]\nfn it_works() {\n    assert!(true);\n}\n' > "$dir/crates/demo/tests/it.rs"
  git -C "$dir" add -A
  git -C "$dir" commit -qm base
  git -C "$dir" tag base
  echo "$dir"
}

commit() { git -C "$1" add -A && git -C "$1" commit -qm pr; }
guard() { (cd "$1" && PR_BODY="$2" bash "$ci/scope_guard.sh" base); }

echo "scope guard"
NONE=$'## Test changes\nnone'
EXPLAINED=$'## Test changes\nRenamed the assertion helper; same checks.'

r="$(new_repo)"; sed -i.bak 's/assert_eq!(super::answer(), 42)/assert!(true)/' "$r/crates/demo/src/lib.rs"; rm "$r"/crates/demo/src/*.bak; commit "$r"
expect "TP-CI-01 editing an inline #[cfg(test)] assertion needs an explanation" 1 guard "$r" "$NONE"
expect "TP-CI-02 ...and passes once explained" 0 guard "$r" "$EXPLAINED"

r="$(new_repo)"; printf '\n#[cfg(test)]\nmod more_tests {\n    #[test]\n    fn new_test() {}\n}\n' >> "$r/crates/demo/src/lib.rs"; commit "$r"
expect "TP-CI-03 adding new tests needs no explanation" 0 guard "$r" "$NONE"

r="$(new_repo)"; sed -i.bak 's/    42/    41 + 1/' "$r/crates/demo/src/lib.rs"; rm "$r"/crates/demo/src/*.bak; commit "$r"
expect "TP-CI-04 editing code above the test module is fine" 0 guard "$r" "$NONE"

r="$(new_repo)"; printf '#[test]\nfn another() {}\n' >> "$r/crates/demo/tests/it.rs"; commit "$r"
expect "TP-CI-05 appending to a test file is fine" 0 guard "$r" "$NONE"

r="$(new_repo)"; sed -i.bak 's/assert!(true)/assert!(1 == 1)/' "$r/crates/demo/tests/it.rs"; rm "$r"/crates/demo/tests/*.bak; commit "$r"
expect "TP-CI-06 changing a line in a test file needs an explanation" 1 guard "$r" "$NONE"

r="$(new_repo)"; git -C "$r" rm -q crates/demo/src/lib.rs; commit "$r"
expect "TP-CI-07 deleting a file that holds tests needs an explanation" 1 guard "$r" "$NONE"

echo "eval gate"
if [ ! -x "$evals_bin" ]; then
  bad "TP-CI-08 corral-evals binary missing at $evals_bin (run: cargo build -p corral-evals)"
else
  r="$(new_repo)"
  mkdir -p "$r/evals/safety" "$r/evals/reports/baselines"
  cp "$repo_root/evals/safety/"*.yaml "$r/evals/safety/"
  "$evals_bin" --suite gate --scenarios "$r/evals/safety" --report "$r/evals/reports/baselines/main.json" >/dev/null
  sed -i.bak 's/"pending"/"passed"/' "$r/evals/reports/baselines/main.json"; rm "$r"/evals/reports/baselines/*.bak
  commit "$r"
  git -C "$r" tag -f base >/dev/null   # main now has a passing EV-SAFE-03 in its baseline
  # The PR deletes the scenario and rewrites its own baseline to hide it.
  git -C "$r" rm -q -r evals/safety
  mkdir -p "$r/evals/safety"
  printf 'id: EV-COST-01\nagent: { adapter: fake, policy_profile: default }\nprompt: x\nsteps:\n  - model_says: hi\nexpect:\n  never:\n    - event: tool.started\n' > "$r/evals/safety/other.yaml"
  printf '{"suite":"gate","scenarios":[],"summary":{},"regressions":[],"gate_passed":true}\n' > "$r/evals/reports/baselines/main.json"
  commit "$r"
  expect "TP-CI-08 the gate uses the base branch's baseline and fails the step" 1 \
    env CORRAL_EVALS_BIN="$evals_bin" bash -c "cd '$r' && bash '$ci/eval_gate.sh' base"
  expect "TP-CI-09 ...while the PR's own baseline alone would have passed" 0 \
    env CORRAL_EVALS_BIN="$evals_bin" bash -c "cd '$r' && bash '$ci/eval_gate.sh'"
fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
