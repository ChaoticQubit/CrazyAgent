set +e -uo pipefail
label=${1:?usage: v244-batch.sh <label>}
out="$RUNNER_TEMP/v244-batch-$label.txt"
table="$RUNNER_TEMP/v244-batch-$label.md"
git log --oneline -1 | tee "$out"
printf '| %s step | exit |\n| --- | --- |\n' "$label" > "$table"
fail=0
step() {
  local name=$1
  shift
  "$@" 2>&1 | tee -a "$out"
  local rc=${PIPESTATUS[0]}
  echo "== $name rc=$rc" | tee -a "$out"
  printf '| %s | %s |\n' "$name" "$rc" >> "$table"
  [ "$rc" = 0 ] || fail=1
}
cargo=(cargo +"$RUST_TOOLCHAIN")
[ "$RUNNER_OS" = Windows ] || { echo "::error::batch-2 is Windows only, not $RUNNER_OS"; exit 1; }
step "build --bin crazyagent-win-boundary" "${cargo[@]}" build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked
mkdir -p target/debug/deps && cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe || fail=1
step "crazyagent test --lib --no-run" "${cargo[@]}" test -p crazyagent --lib --locked --no-run
step "windows_tests of the other touched modules" "${cargo[@]}" test -p crazyagent --lib --locked --no-fail-fast -- agent_home::windows_tests github::state::windows_tests harnesses::update::state::windows_tests instruction_files::windows_tests research::runner::windows_tests usage::capture::windows_tests
failed=$(awk '/^test .* \.\.\. FAILED$/ {print $2}' "$out" | sort -u)
ran=$(grep -cE '^test .* \.\.\. (ok|FAILED|ignored)$' "$out")
{
  cat "$table"
  echo
  echo "Rows run: $ran. Failed rows: $(printf '%s' "$failed" | grep -c .)"
  [ -z "$failed" ] || printf -- '- %s\n' $failed
} >> "$GITHUB_STEP_SUMMARY"
[ "$ran" -gt 0 ] || { echo "::error::the filters matched no rows"; fail=1; }
exit "$fail"
