set +e -uo pipefail
label=${1:?usage: ledger-unix.sh <label>}
out="$RUNNER_TEMP/ledger-unix-$label.txt"
git log --oneline -1 | tee "$out"
printf '| %s step | exit |\n| --- | --- |\n' "$label" >> "$GITHUB_STEP_SUMMARY"
fail=0
step() {
  local name=$1
  shift
  "$@" 2>&1 | tee -a "$out"
  local rc=${PIPESTATUS[0]}
  echo "== $name rc=$rc" | tee -a "$out"
  printf '| %s | %s |\n' "$name" "$rc" >> "$GITHUB_STEP_SUMMARY"
  [ "$rc" = 0 ] || fail=1
}
cargo=(cargo +"$RUST_TOOLCHAIN")
step "crazyagent lib build" "${cargo[@]}" test -p crazyagent --lib --locked --no-run
step "ledger:: rows" "${cargo[@]}" test -p crazyagent --lib --locked -- ledger::
step "clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
exit "$fail"
