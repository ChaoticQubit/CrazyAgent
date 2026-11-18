#!/usr/bin/env bash
set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
tip=c5a3835b1e3336a7d1de08586fae765b01839fe4
base=5e4168cdcb7d976ac1835a720a8a75da2f924a59
failed=0
note() { printf '%s\n' "$1" | tee -a "$summary"; }
at() {
  git checkout -q -f --detach "$1" || { echo "checkout $1 failed"; exit 1; }
  echo "######## at $(git log --oneline -1)"
}
helpers() {
  cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/bins.txt" 2>&1 || { tail -n 20 "$RUNNER_TEMP/bins.txt"; return 1; }
  cargo build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked > "$RUNNER_TEMP/boundary.txt" 2>&1 || { tail -n 20 "$RUNNER_TEMP/boundary.txt"; return 1; }
  mkdir -p target/debug/deps
  cp target/debug/crazyagent-landlock.exe target/debug/deps/crazyagent-landlock.exe
  cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe
  cargo test -p crazyagent --locked --lib --no-run > "$RUNNER_TEMP/lib.txt" 2>&1 || { grep -E '^(error|warning)' -A 6 "$RUNNER_TEMP/lib.txt" | head -n 80; return 1; }
}
note "| ${ImageOS:-windows} full engine --lib | result |"
note "| --- | --- |"
at "$tip"
helpers || { note "| build at tip | FAILED |"; echo "FARM failed=1"; exit 1; }
cargo test -p crazyagent --locked --lib > "$RUNNER_TEMP/suite.txt" 2>&1
rc=$?
grep -E '^test result:' "$RUNNER_TEMP/suite.txt"
grep -E '^test .* \.\.\. FAILED$' "$RUNNER_TEMP/suite.txt" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sed -E 's/ - should panic$//' > "$RUNNER_TEMP/failed.txt"
count=$(grep -c . "$RUNNER_TEMP/failed.txt")
unreached=$(grep -cF 'cannot read the read root' "$RUNNER_TEMP/suite.txt")
note "| at tip | exit $rc, $count failed rows, $unreached lines naming an unreached read root |"
grep -nF 'cannot read the read root' "$RUNNER_TEMP/suite.txt" | head -n 40
sed -n '/^failures:$/,$p' "$RUNNER_TEMP/suite.txt" | head -n 400
if [ "$count" -gt 0 ]; then
  at "$base"
  helpers || { note "| build at base | FAILED |"; echo "FARM failed=1"; exit 1; }
  while read -r name; do
    cargo test -p crazyagent --locked --lib -- --exact "$name" --test-threads 1 > "$RUNNER_TEMP/again.txt" 2>&1
    if grep -qE "^test $name( - should panic)? \.\.\. FAILED$" "$RUNNER_TEMP/again.txt"; then
      note "| $name | red at tip and at base $base: not this branch |"
    else
      note "| $name | **red at tip only** |"; failed=1
    fi
  done < "$RUNNER_TEMP/failed.txt"
fi
[ "$unreached" -eq 0 ] || failed=1
[ "$rc" -eq 0 ] || [ "$count" -gt 0 ] || failed=1
echo "FARM failed=$failed"
exit "$failed"
