set +e -uo pipefail
label=${1:?usage: sync-compile.sh <label>}
out="$RUNNER_TEMP/sync-compile-$label.txt"
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
features=()
if grep -Eq '^owner-trace[[:space:]]*=' crates/crazyagent/Cargo.toml; then features=(--features crazyagent/owner-trace); fi
case "$RUNNER_OS" in
  Linux)
    step "desktop npm ci" npm --prefix apps/desktop ci --ignore-scripts --no-audit --no-fund
    step "desktop typecheck" npm --prefix apps/desktop run typecheck
    step "desktop build" npm --prefix apps/desktop run build
    step "desktop renderer policy rows" node --test apps/desktop/test/terminal-renderer-policy.test.mjs
    step "build --bin crazyagent --bin crazyagent-landlock" "${cargo[@]}" build -p crazyagent --bin crazyagent --bin crazyagent-landlock "${features[@]}" --locked
    step "crazyagent lib tests build" "${cargo[@]}" test -p crazyagent --lib "${features[@]}" --locked --no-run
    step "clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  Windows)
    step "build --bin crazyagent-win-boundary" "${cargo[@]}" build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked
    step "build --bin crazyagent" "${cargo[@]}" build -p crazyagent --bin crazyagent "${features[@]}" --locked
    step "crazyagent lib tests build" "${cargo[@]}" test -p crazyagent --lib "${features[@]}" --locked --no-run
    step "crazyagent clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "crazyagent clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    step "win-sandbox clippy --tests" "${cargo[@]}" clippy -p crazyagent-win-sandbox --tests --locked --keep-going -- -D warnings
    ;;
  macOS)
    step "build --lib" "${cargo[@]}" build -p crazyagent --lib --locked
    step "clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  *)
    echo "::error::no compile recipe for RUNNER_OS=$RUNNER_OS"
    exit 1
    ;;
esac
exit "$fail"
