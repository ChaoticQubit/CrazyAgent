set +e -uo pipefail
label=${1:?usage: link-cli.sh <label>}
out="$RUNNER_TEMP/link-cli-$label.txt"
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
  Windows)
    export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$GITHUB_WORKSPACE/target/debug/crazyagent.exe"
    filter=$(node scripts/ci/nextest-filter.mjs) || { echo "::error::no trunk nextest filter"; exit 1; }
    lib=(nextest run -p crazyagent --lib "${features[@]}" --locked --profile farm -E "package(=crazyagent) & kind(lib) & ($filter) & test(/^service::/)")
    step "build --bin crazyagent-win-boundary" "${cargo[@]}" build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked
    mkdir -p target/debug/deps && cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe || fail=1
    step "build --bin crazyagent" "${cargo[@]}" build -p crazyagent --bin crazyagent "${features[@]}" --locked
    step "crazyagent lib build" "${cargo[@]}" "${lib[@]}" --no-run
    for try in 1 2 3; do
      step "service:: rows, try $try" "${cargo[@]}" "${lib[@]}" --no-fail-fast
    done
    step "crazyagent clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "crazyagent clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  Linux)
    step "clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  *)
    echo "::error::no recipe for RUNNER_OS=$RUNNER_OS"
    exit 1
    ;;
esac
tr -d '\r' < "$out" | grep -E '^[[:space:]]*(FAIL|TIMEOUT|SIGSEGV|SIGABRT|SIGKILL|SIGTERM|ABORT|LEAK-FAIL) \[' | awk '{print $NF}' | sort | uniq -c >> "$GITHUB_STEP_SUMMARY"
exit "$fail"
