#!/bin/bash
set -uo pipefail
out="$RUNNER_TEMP/win-tip"
mkdir -p "$out"
echo "windows at $(git rev-parse --short=9 HEAD)"
bad=0
cargo clippy -p crazyagent --lib --locked -- -D warnings > "$out/clippy-lib.txt" 2>&1 && echo "CLIPPY windows lib clean" || { grep -E '^(error|warning)' -A12 "$out/clippy-lib.txt" | head -120; echo "::error::Windows clippy on the lib"; bad=1; }
cargo clippy -p crazyagent --lib --profile test --locked -- -D warnings > "$out/clippy-test.txt" 2>&1 && echo "CLIPPY windows lib test clean" || { grep -E '^(error|warning)' -A12 "$out/clippy-test.txt" | head -120; echo "::error::Windows clippy on the lib test build"; bad=1; }
cargo test -p crazyagent --lib --locked --no-run > "$out/build.txt" 2>&1 || { grep -E '^(error|warning)' -A10 "$out/build.txt" | head -80; echo "::error::Windows lib test build failed"; exit 1; }
row=storage::narrowing::windows_tests::where_the_intent_store_has_no_private_folder_judge_it_opens_as_no_store_and_makes_nothing
for i in 1 2 3; do
  cargo test -p crazyagent --lib --locked -- --exact "$row" > "$out/row-$i.txt" 2>&1
  code=$?
  grep -qE "^test $row \.\.\. (ok|FAILED)" "$out/row-$i.txt" || { echo "::error::the intent-store row did not run (try $i)"; tail -30 "$out/row-$i.txt"; bad=1; continue; }
  if [ "$code" -eq 0 ]; then echo "GREEN: ${row##*::} (try $i)"; else grep -hE 'panicked at' -A8 "$out/row-$i.txt" | head -12; echo "::error::the intent-store row failed (try $i)"; bad=1; fi
done
cargo test -p crazyagent --lib --locked -- storage:: > "$out/storage.txt" 2>&1
echo "STORAGE SUITE $(grep -E '^test result' "$out/storage.txt" | tail -1)"
grep -E '^test .* FAILED' "$out/storage.txt" | head -30
grep -E '^---- .* stdout ----' -A10 "$out/storage.txt" | grep -E 'panicked at|left:|right:' | head -30
grep -qE '^test .* FAILED' "$out/storage.txt" && { echo "::error::Windows storage suite reds at the tip"; bad=1; }
exit "$bad"
