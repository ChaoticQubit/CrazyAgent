set -uo pipefail
kind=${1:?host kind: linux or windows}
tip=d9269f059
red=709c67d6c
out="$RUNNER_TEMP/v244-win-signin-n6a-$kind.txt"
failed=0
: > "$out"
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" | tee -a "$out"; }
git diff --quiet "$tip" -- crates Cargo.lock Cargo.toml || { row "code tip" "$tip" "NOT: crates differ"; exit 1; }
row "code tip" "$tip" "crates, Cargo.toml and Cargo.lock equal"
row "host" "$kind" "$(uname -srm)"
module=engine::bootstrap::native_profiles::custody_windows_tests
ran() {
  local label=$1 file=$2
  local result reds
  result=$(grep -E '^test result:' "$file" | tail -1)
  reds=$(grep -E '^test .* FAILED$' "$file" | awk '{print $2}' | tr '\n' ' ')
  if [ -n "$result" ] && [ -z "$reds" ] && ! grep -qE '^test result: FAILED' "$file"; then row "$label" "0 failed" "$result"; else
    row "$label" "0 failed" "NOT: $result $reds"
    grep -A4 'panicked at' "$file" | head -40 | cut -c1-300 | tee -a "$out"
    failed=1
  fi
}
listed() { cargo test -p crazyagent --lib --locked -- --list "$1" 2>/dev/null | grep -cE ': test$' || true; }
if [ "$kind" = windows ]; then
  git checkout -q "$red" -- crates
  red_rows=$(listed "$module::")
  cargo test -p crazyagent --lib --locked -- --test-threads 1 "$module::" > "$RUNNER_TEMP/red-$kind.txt" 2>&1
  git reset -q --hard HEAD
  git clean -fdq -- crates
  git diff --quiet "$tip" -- crates && [ -z "$(git status --porcelain -- crates)" ] || { row "restore after red" "$tip" NOT; exit 1; }
  if [ "$red_rows" = 7 ] && grep -qE '^test result: FAILED\. 1 passed; 6 failed' "$RUNNER_TEMP/red-$kind.txt"; then row "red rows at $red ($red_rows listed)" "1 passed, 6 failed" "1 passed, 6 failed"; else
    row "red rows at $red ($red_rows listed)" "7 listed, 1 passed, 6 failed" "NOT: $(grep -E '^test result' "$RUNNER_TEMP/red-$kind.txt" | tail -1)"
    tail -40 "$RUNNER_TEMP/red-$kind.txt" | tee -a "$out"
    failed=1
  fi
  grep -qE "^test $module::a_capture_from_metadata_is_refused_on_windows \.\.\. ok$" "$RUNNER_TEMP/red-$kind.txt" || { row "red pin row" ok NOT; failed=1; }
  unavailable=$(grep -c 'native_profile_custody_unavailable' "$RUNNER_TEMP/red-$kind.txt" || true)
  [ "${unavailable:-0}" -ge 6 ] && row "red reason" "custody_unavailable x6" "$unavailable lines" || { row "red reason" "custody_unavailable x6" "NOT: $unavailable"; failed=1; }
  grep -E -A3 'panicked at' "$RUNNER_TEMP/red-$kind.txt" | grep -vE '^--$|RUST_BACKTRACE' | head -30 | cut -c1-260 | tee -a "$out"
  for target in --lib --tests; do
    log="$RUNNER_TEMP/clippy$target-$kind.txt"
    cargo clippy -p crazyagent "$target" --locked -- -D warnings > "$log" 2>&1
    rc=$?
    if [ "$rc" -eq 0 ]; then row "clippy -p crazyagent $target -D warnings" "rc 0" "rc 0"; else
      row "clippy -p crazyagent $target -D warnings" "rc 0" "NOT: rc $rc"
      grep -E -A10 '^(warning|error)' "$log" | head -80 | cut -c1-300 | tee -a "$out"
      failed=1
    fi
  done
  tip_rows=$(listed "$module::")
  cargo test -p crazyagent --lib --locked -- --test-threads 1 "$module::" > "$RUNNER_TEMP/tip-$kind.txt" 2>&1
  ran "N6a custody rows at $tip ($tip_rows listed)" "$RUNNER_TEMP/tip-$kind.txt"
  grep -qE '^test result: ok\. 10 passed; 0 failed' "$RUNNER_TEMP/tip-$kind.txt" || { row "N6a rows count" "10 passed" NOT; failed=1; }
fi
rows=$(listed engine::bootstrap::)
cargo test -p crazyagent --lib --locked -- --test-threads 1 engine::bootstrap:: > "$RUNNER_TEMP/bootstrap-$kind.txt" 2>&1
ran "lib engine::bootstrap:: ($rows listed) at $tip" "$RUNNER_TEMP/bootstrap-$kind.txt"
[ "${rows:-0}" -gt 10 ] || { row "bootstrap rows listed" "> 10" "NOT: $rows"; failed=1; }
if [ "$kind" = linux ]; then
  cargo clippy -p crazyagent --lib --locked -- -D warnings > "$RUNNER_TEMP/clippy-lib-$kind.txt" 2>&1 && row "clippy --lib -D warnings" "rc 0" "rc 0" || { row "clippy --lib -D warnings" "rc 0" NOT; grep -E -A8 '^(warning|error)' "$RUNNER_TEMP/clippy-lib-$kind.txt" | head -40 | tee -a "$out"; failed=1; }
fi
echo "N6a $kind failed=$failed" | tee -a "$out"
exit "$failed"
