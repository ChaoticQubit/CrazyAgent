set +e -uo pipefail
label=${1:?usage: ledger-measure.sh <label>}
out="$RUNNER_TEMP/ledger-measure-$label.txt"
summary="$RUNNER_TEMP/ledger-summary.md"
: > "$out"
[ "$RUNNER_OS" = Windows ] || { echo "::error::Windows only, not $RUNNER_OS"; exit 1; }
row=ledger::tests::pending_actions_are_bounded_and_finished_ones_are_pruned_to_the_kept_count
cargo=(cargo +"$RUST_TOOLCHAIN")
export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$GITHUB_WORKSPACE/target/debug/crazyagent.exe"
tries=5
fail=0
printf '| %s tree | case | row result and time |\n| --- | --- | --- |\n' "$label" > "$summary"
timed() {
  tr -d '\r' < "$1" | grep -E "^[[:space:]]*(PASS|FAIL|TIMEOUT|SIGKILL|SIGTERM|ABORT) \[ *[0-9.]+s\].*${row}\$" | tail -1 | sed -E 's/^[[:space:]]*([A-Z-]+) \[ *([0-9.]+)s\].*/\1 \2/'
}
for sha in dc32643dd cc3399155; do
  git checkout -q --detach "$sha" || { echo "::error::cannot check out $sha"; exit 1; }
  git log --oneline -1 | tee -a "$out"
  features=()
  if grep -Eq '^owner-trace[[:space:]]*=' crates/crazyagent/Cargo.toml; then features=(--features crazyagent/owner-trace); fi
  filter=$(node scripts/ci/nextest-filter.mjs) || { echo "::error::no trunk nextest filter at $sha"; exit 1; }
  lib=(nextest run -p crazyagent --lib "${features[@]}" --locked --profile farm)
  "${cargo[@]}" build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked 2>&1 | tail -2 | tee -a "$out"
  mkdir -p target/debug/deps && cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe
  "${cargo[@]}" build -p crazyagent --bin crazyagent "${features[@]}" --locked 2>&1 | tail -2 | tee -a "$out"
  "${cargo[@]}" "${lib[@]}" --no-run 2>&1 | tail -3 | tee -a "$out"
  for try in $(seq 1 "$tries"); do
    "${cargo[@]}" "${lib[@]}" -E "package(=crazyagent) & kind(lib) & test(=$row)" > "$RUNNER_TEMP/alone.txt" 2>&1
    tr -d '\r' < "$RUNNER_TEMP/alone.txt" >> "$out"
    got=$(timed "$RUNNER_TEMP/alone.txt")
    echo "== $sha alone try $try: ${got:-no result}" | tee -a "$out"
    printf '| %s | alone, try %s | %s |\n' "$sha" "$try" "${got:-no result}" >> "$summary"
    if [ "$sha" = cc3399155 ] && [ "${got%% *}" != PASS ]; then fail=1; fi
  done
  "${cargo[@]}" "${lib[@]}" --no-fail-fast -E "package(=crazyagent) & kind(lib) & ($filter)" > "$RUNNER_TEMP/full.txt" 2>&1
  tr -d '\r' < "$RUNNER_TEMP/full.txt" >> "$out"
  got=$(timed "$RUNNER_TEMP/full.txt")
  run=$(tr -d '\r' < "$RUNNER_TEMP/full.txt" | grep -E '^[[:space:]]+Summary \[' | tail -1 | sed -E 's/^[[:space:]]+//')
  echo "== $sha under the full lib: ${got:-no result}; $run" | tee -a "$out"
  printf '| %s | under the full lib (%s) | %s |\n' "$sha" "$run" "${got:-no result}" >> "$summary"
  if [ "$sha" = cc3399155 ] && [ "${got%% *}" != PASS ]; then fail=1; fi
  if [ "$sha" = cc3399155 ]; then
    "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings 2>&1 | tail -3 | tee -a "$out"
    rc=${PIPESTATUS[0]}
    echo "== $sha clippy --tests rc=$rc" | tee -a "$out"
    printf '| %s | clippy --tests | rc %s |\n' "$sha" "$rc" >> "$summary"
    [ "$rc" = 0 ] || fail=1
  fi
done
cat "$summary" >> "$GITHUB_STEP_SUMMARY"
exit "$fail"
