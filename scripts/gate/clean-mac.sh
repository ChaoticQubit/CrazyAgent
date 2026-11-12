#!/bin/bash
set -uo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
gate="${CRAZYAGENT_CLEAN_MAC_DIR:-$repo/.local-state/gate/clean-mac/$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$gate"
gate="$(cd "$gate" && pwd)"
summary="$gate/summary.txt"
: > "$summary"

refuse() {
  printf 'clean-mac: REFUSED, %s\n' "$1" | tee -a "$summary"
  exit 2
}

[ "$(uname -s)" = "Darwin" ] || refuse "macOS only"
grep -Eq '^clean-mac-gate[[:space:]]*=' "$repo/crates/crazyagent/Cargo.toml" \
  || refuse "the engine has no clean-mac-gate feature (system-root remap), so a run would see this Mac's real Homebrew"
TMPDIR="$(getconf DARWIN_USER_TEMP_DIR)" || refuse "no private temp folder for this user"
export TMPDIR

log="$gate/cargo-test.log"
CRAZYAGENT_CLEAN_MAC_DIR="$gate" cargo test --manifest-path "$repo/Cargo.toml" -p crazyagent \
  --features clean-mac-gate --locked --test clean_mac -- --include-ignored --test-threads=1 > "$log" 2>&1
rc=$?

if grep -q 'error\[E' "$log"; then
  printf 'clean-mac: BUILD FAILED, %s compile errors, no shape ran (see %s)\n' "$(grep -c 'error\[E' "$log")" "$log" | tee -a "$summary"
  exit 3
fi

pass=$(grep -c ': PASS$' "$summary")
fail=$(grep -c ': FAIL' "$summary")
{
  printf 'rc=%s pass=%s fail=%s shapes=4\n' "$rc" "$pass" "$fail"
  grep -m 1 'gate refused' "$log"
  grep -A 3 'panicked at' "$log" | head -n 60
} >> "$summary"
cat "$summary"

grep -q 'gate refused' "$log" && exit 2
[ "$rc" -eq 0 ] && [ "$fail" -eq 0 ] && [ "$pass" -eq 4 ] && exit 0
exit 1
