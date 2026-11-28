#!/usr/bin/env bash
# The release check (Spike S2, TP-CI-17..19): a binary built with the `e2e` feature (scripted fake
# providers, keys in memory; apps/desktop/src-tauri/src/e2e.rs) must never ship. That build prints
# a mark when it starts (`e2e::MARKER`), so its binary carries the mark's bytes, stripped or not.
# Fails if the binary has them, and fails closed if it can't be read.
# Run: scripts/ci/no_e2e_in_release.sh <binary>
set -euo pipefail

MARK='corral-e2e-build'

bin="${1:-}"
if [ -z "$bin" ]; then
  echo "usage: $0 <binary>" >&2
  exit 2
fi
if [ ! -f "$bin" ] || [ ! -r "$bin" ]; then
  echo "no e2e check: can't read $bin" >&2
  exit 2
fi

# grep: 0 found, 1 not found, anything else an error. Only "not found" passes.
status=0
grep -a -q -F -- "$MARK" "$bin" || status=$?
case "$status" in
  0) echo "FAIL: $bin is an e2e build (it carries '$MARK'): fake providers, keys in memory." >&2
     exit 1 ;;
  1) echo "ok: $bin isn't an e2e build" ;;
  *) echo "no e2e check: grep couldn't read $bin (exit $status)" >&2
     exit 2 ;;
esac
