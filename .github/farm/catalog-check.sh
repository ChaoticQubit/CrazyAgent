set +e -uo pipefail
label=${1:?usage: catalog-check.sh <label>}
red=${2:?usage: catalog-check.sh <label> <red sha> <fix sha>}
fix=${3:?usage: catalog-check.sh <label> <red sha> <fix sha>}
out="$RUNNER_TEMP/catalog-check-$label.txt"
table="$RUNNER_TEMP/catalog-check-$label.md"
git log --oneline -3 | tee "$out"
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
expect_red() {
  local name=$1
  shift
  "$@" 2>&1 | tee -a "$out"
  local rc=${PIPESTATUS[0]}
  echo "== $name rc=$rc (red expected)" | tee -a "$out"
  printf '| %s (red expected) | %s |\n' "$name" "$rc" >> "$table"
  [ "$rc" != 0 ] || fail=1
}
keyring() {
  dbus-run-session -- bash -euo pipefail -c 'printf "%s\n" ci-keyring | gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null; exec "$@"' farm "$@"
}
at() {
  git -c advice.detachedHead=false checkout -q --detach "$1" && git log --oneline -1 | tee -a "$out"
}
cargo=(cargo +"$RUST_TOOLCHAIN")
features=()
if grep -Eq '^owner-trace[[:space:]]*=' crates/crazyagent/Cargo.toml; then features=(--features crazyagent/owner-trace); fi
rows='test(=engine::owner::transport::owner_deadline_tests::a_catalog_check_slower_than_the_owner_deadline_and_the_request_timeout_still_answers) | test(=engine::owner::transport::harness_catalog::updates::tests::list_stays_on_the_inline_path_and_check_is_served_past_the_deadline)'
near='test(/^engine::owner::transport::owner_deadline_tests::/) | test(/^engine::owner::transport::harness_catalog::/)'
named=(nextest run -p crazyagent --lib "${features[@]}" --locked --profile farm)
case "$RUNNER_OS" in
  Linux)
    export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$GITHUB_WORKSPACE/target/debug/crazyagent"
    at "$red"
    step "red: lib build" "${cargo[@]}" "${named[@]}" --no-run
    expect_red "red: the two new rows" keyring "${cargo[@]}" "${named[@]}" --no-fail-fast -E "$rows"
    at "$fix"
    step "build --bin crazyagent --bin crazyagent-landlock" "${cargo[@]}" build -p crazyagent --bin crazyagent --bin crazyagent-landlock "${features[@]}" --locked
    mkdir -p target/debug/deps && cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock || fail=1
    step "fix: lib build" "${cargo[@]}" "${named[@]}" --no-run
    for try in 1 2 3; do
      step "fix: the two new rows, try $try" keyring "${cargo[@]}" "${named[@]}" --no-fail-fast -E "$rows"
    done
    step "fix: every owner_deadline_tests and harness_catalog row" keyring "${cargo[@]}" "${named[@]}" --no-fail-fast -E "$near"
    e2e=(nextest run -p crazyagent --test e2e_tui "${features[@]}" --locked --profile farm -E 'binary_id(=crazyagent::e2e_tui) & test(/master_harness_that/)')
    step "fix: e2e_tui build" "${cargo[@]}" "${e2e[@]}" --no-run
    for try in 1 2 3; do
      step "fix: e2e_tui master picker rows, try $try" keyring "${cargo[@]}" "${e2e[@]}" --no-fail-fast
    done
    step "fix: clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "fix: clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  Windows)
    at "$fix"
    step "fix: crazyagent clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "fix: crazyagent clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  *)
    echo "::error::no recipe for RUNNER_OS=$RUNNER_OS"
    exit 1
    ;;
esac
tr -d '\r' < "$out" > "$out.lf"
failed=$(grep -E '^[[:space:]]*(FAIL|TIMEOUT|SIGSEGV|SIGABRT|SIGKILL|SIGTERM|ABORT|LEAK-FAIL) \[' "$out.lf" | awk '{print $NF}' | sort | uniq -c)
{
  cat "$table"
  echo
  echo "Failed rows, per name with count:"
  printf '%s\n' "$failed"
} >> "$GITHUB_STEP_SUMMARY"
exit "$fail"
