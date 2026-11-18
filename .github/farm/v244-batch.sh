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
keyring() {
  dbus-run-session -- bash -euo pipefail -c 'printf "%s\n" ci-keyring | gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null; exec "$@"' farm "$@"
}
cargo=(cargo +"$RUST_TOOLCHAIN")
features=()
if grep -Eq '^owner-trace[[:space:]]*=' crates/crazyagent/Cargo.toml; then features=(--features crazyagent/owner-trace); fi
exe=""
[ "$RUNNER_OS" != Windows ] || exe=.exe
export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$GITHUB_WORKSPACE/target/debug/crazyagent$exe"
lib=()
if [ "$RUNNER_OS" != macOS ]; then
  filter=$(node scripts/ci/nextest-filter.mjs) || { echo "::error::no trunk nextest filter"; exit 1; }
  lib=(nextest run -p crazyagent --lib "${features[@]}" --locked --profile farm -E "package(=crazyagent) & kind(lib) & ($filter)")
fi
case "$RUNNER_OS" in
  Windows)
    step "win-sandbox test --lib" "${cargo[@]}" test -p crazyagent-win-sandbox --lib --locked --no-fail-fast
    for target in boundary private_judge private_windows; do
      step "win-sandbox test --test $target" "${cargo[@]}" test -p crazyagent-win-sandbox --test "$target" --locked --no-fail-fast
    done
    step "build --bin crazyagent-win-boundary" "${cargo[@]}" build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked
    mkdir -p target/debug/deps && cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe || fail=1
    step "build --bin crazyagent" "${cargo[@]}" build -p crazyagent --bin crazyagent "${features[@]}" --locked
    step "crazyagent lib build" "${cargo[@]}" "${lib[@]}" --no-run
    step "crazyagent lib, every row trunk runs" "${cargo[@]}" "${lib[@]}" --no-fail-fast
    step "crazyagent clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "crazyagent clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    step "win-sandbox clippy --lib" "${cargo[@]}" clippy -p crazyagent-win-sandbox --lib --locked -- -D warnings
    step "win-sandbox clippy --bins" "${cargo[@]}" clippy -p crazyagent-win-sandbox --bins --locked -- -D warnings
    step "win-sandbox clippy --tests" "${cargo[@]}" clippy -p crazyagent-win-sandbox --tests --locked --keep-going -- -D warnings
    ;;
  Linux)
    step "build --bin crazyagent --bin crazyagent-landlock" "${cargo[@]}" build -p crazyagent --bin crazyagent --bin crazyagent-landlock "${features[@]}" --locked
    mkdir -p target/debug/deps && cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock || fail=1
    step "crazyagent lib build" "${cargo[@]}" "${lib[@]}" --no-run
    step "crazyagent lib, every row trunk runs, under a keyring session" keyring "${cargo[@]}" "${lib[@]}" --no-fail-fast
    step "clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  macOS)
    step "build --lib" "${cargo[@]}" build -p crazyagent --lib --locked
    step "clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  *)
    echo "::error::no batch recipe for RUNNER_OS=$RUNNER_OS"
    exit 1
    ;;
esac
tr -d '\r' < "$out" > "$out.lf"
failed=$( { awk '/^test .* \.\.\. FAILED$/ {print $2}' "$out.lf"; grep -E '^[[:space:]]*(FAIL|TIMEOUT|SIGSEGV|SIGABRT|SIGKILL|SIGTERM|ABORT|LEAK-FAIL) \[' "$out.lf" | awk '{print $NF}'; } | sort -u)
{
  cat "$table"
  echo
  echo "Failed rows: $(printf '%s' "$failed" | grep -c .)"
  [ -z "$failed" ] || printf -- '- %s\n' $failed
} >> "$GITHUB_STEP_SUMMARY"
exit "$fail"
