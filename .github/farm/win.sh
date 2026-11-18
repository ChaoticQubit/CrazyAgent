#!/bin/bash
set -uo pipefail
label=$1
red=$2
green=$3
shift 3
out="$RUNNER_TEMP/win-$label"
mkdir -p "$out"
bad=0
build() {
  git checkout -q --detach "$1" || { echo "::error::checkout $1 failed"; exit 1; }
  echo "$label $2 at $(git rev-parse --short=9 HEAD)"
  cargo test -p crazyagent --lib --locked --no-run > "$out/build-$2.txt" 2>&1 || { grep -E '^(error|warning)' -A10 "$out/build-$2.txt" | head -80; echo "::error::$label lib test build failed at $2"; exit 1; }
}
rows() {
  for row in "$@"; do
    short=${row##*::}
    for i in 1 2 3; do
      cargo test -p crazyagent --lib --locked -- --exact "$row" > "$out/$phase-$short-$i.txt" 2>&1
      code=$?
      grep -qE "^test $row \.\.\. (ok|FAILED)" "$out/$phase-$short-$i.txt" || { echo "::error::$short did not run at $phase (try $i)"; tail -30 "$out/$phase-$short-$i.txt"; bad=1; continue; }
      if [ "$phase" = red ]; then
        if [ "$code" -eq 0 ]; then echo "::error::$short passed before the fix (try $i)"; bad=1;
        elif grep -q narrowing_intents_unavailable "$out/$phase-$short-$i.txt"; then echo "RED for the named reason: $short (try $i)";
        else grep -hE 'panicked at' -A6 "$out/$phase-$short-$i.txt" | head -10; echo "::error::$short failed for another reason (try $i)"; bad=1; fi
      else
        if [ "$code" -eq 0 ]; then echo "GREEN: $short (try $i)"; else grep -hE 'panicked at' -A8 "$out/$phase-$short-$i.txt" | head -12; echo "::error::$short failed at the fix (try $i)"; bad=1; fi
      fi
    done
  done
}
phase=red
build "$red" red
rows "$@"
phase=green
build "$green" green
rows "$@"
if [ "$label" = b ]; then
  cargo test -p crazyagent --lib --locked -- storage::windows_tests:: storage::narrowing::windows_tests:: > "$out/suite.txt" 2>&1
  echo "SUITE $(grep -E '^test result' "$out/suite.txt" | tail -1)"
  grep -E '^test .* FAILED' "$out/suite.txt" | head -20
  grep -qE '^test .* FAILED' "$out/suite.txt" && { echo "::error::Windows storage suite reds at the fix"; bad=1; }
else
  cargo clippy -p crazyagent --lib --locked -- -D warnings > "$out/clippy-lib.txt" 2>&1 && echo "CLIPPY windows lib clean" || { grep -E '^(error|warning)' -A12 "$out/clippy-lib.txt" | head -120; echo "::error::Windows clippy on the lib"; bad=1; }
  cargo clippy -p crazyagent --lib --profile test --locked -- -D warnings > "$out/clippy-test.txt" 2>&1 && echo "CLIPPY windows lib test clean" || { grep -E '^(error|warning)' -A12 "$out/clippy-test.txt" | head -120; echo "::error::Windows clippy on the lib test build"; bad=1; }
fi
exit "$bad"
