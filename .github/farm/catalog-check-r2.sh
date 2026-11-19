set +e -uo pipefail
label=${1:?usage: catalog-check-r2.sh <label> <red sha> <fix sha> <timeline sha>}
red=${2:?usage: catalog-check-r2.sh <label> <red sha> <fix sha> <timeline sha>}
fix=${3:?usage: catalog-check-r2.sh <label> <red sha> <fix sha> <timeline sha>}
timeline=${4:?usage: catalog-check-r2.sh <label> <red sha> <fix sha> <timeline sha>}
out="$RUNNER_TEMP/catalog-check-$label.txt"
table="$RUNNER_TEMP/catalog-check-$label.md"
git log --oneline -4 | tee "$out"
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
known_red() {
  local name=$1
  shift
  "$@" 2>&1 | tee -a "$out"
  local rc=${PIPESTATUS[0]}
  echo "== $name rc=$rc (carried red allowed)" | tee -a "$out"
  printf '| %s (carried red allowed) | %s |\n' "$name" "$rc" >> "$table"
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
wait_row='test(=engine::owner::transport::harness_catalog::tests::a_check_waits_thirty_seconds_and_a_list_its_request_timeout)'
r1_rows='test(=engine::owner::transport::owner_deadline_tests::a_catalog_check_slower_than_the_owner_deadline_and_the_request_timeout_still_answers) | test(=engine::owner::transport::harness_catalog::updates::tests::list_stays_on_the_inline_path_and_check_is_served_past_the_deadline)'
near='test(/^engine::owner::transport::owner_deadline_tests::/) | test(/^engine::owner::transport::harness_catalog::/)'
queue_row='test(=bridge::queue::tests::a_call_that_lets_the_bridge_go_leaves_changes_free_while_it_runs)'
timeline_row='test(=project_setup::engine_tests::message_timeline::with_owner_items_hidden_the_timeline_shows_only_the_master_s_words)'
lib=(nextest run -p crazyagent --lib "${features[@]}" --locked --profile farm)
desk=(nextest run -p crazyagent-desktop --lib --locked --profile farm)
bins() {
  step "$1: build --bin crazyagent --bin crazyagent-landlock" "${cargo[@]}" build -p crazyagent --bin crazyagent --bin crazyagent-landlock "${features[@]}" --locked
  mkdir -p target/debug/deps && cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock || fail=1
}
case "$RUNNER_OS" in
  Linux)
    export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$GITHUB_WORKSPACE/target/debug/crazyagent"
    at "$fix"
    step "desktop npm ci" npm --prefix apps/desktop ci --ignore-scripts --no-audit --no-fund
    step "desktop build" npm --prefix apps/desktop run build
    step "fix: bridge-routes.test.mjs (routes.rs regenerated)" node --test apps/desktop/test/native/bridge-routes.test.mjs
    at "$red"
    step "red: crazyagent lib build" "${cargo[@]}" "${lib[@]}" --no-run
    expect_red "red: the 30 s wait row" keyring "${cargo[@]}" "${lib[@]}" --no-fail-fast -E "$wait_row"
    step "red: desktop lib build" "${cargo[@]}" "${desk[@]}" --no-run
    expect_red "red: the desktop queue row" "${cargo[@]}" "${desk[@]}" --no-fail-fast -E "$queue_row"
    at "$fix"
    bins fix
    step "fix: crazyagent lib build" "${cargo[@]}" "${lib[@]}" --no-run
    for try in 1 2 3; do
      step "fix: the 30 s wait row and both r1 rows, try $try" keyring "${cargo[@]}" "${lib[@]}" --no-fail-fast -E "$wait_row | $r1_rows"
    done
    known_red "fix: every owner_deadline_tests and harness_catalog row (skill import row = V243-STORAGE-BUSY-ROWS)" keyring "${cargo[@]}" "${lib[@]}" --no-fail-fast -E "$near"
    step "fix: desktop lib build" "${cargo[@]}" "${desk[@]}" --no-run
    for try in 1 2 3; do
      step "fix: every desktop queue row, try $try" "${cargo[@]}" "${desk[@]}" --no-fail-fast -E 'test(/^bridge::queue::tests::/)'
    done
    step "fix: desktop --test harness_catalog_bridge" "${cargo[@]}" nextest run -p crazyagent-desktop --test harness_catalog_bridge --locked --profile farm --no-fail-fast
    e2e=(nextest run -p crazyagent --test e2e_tui "${features[@]}" --locked --profile farm -E 'binary_id(=crazyagent::e2e_tui) & test(/master_harness_that/)')
    step "fix: e2e_tui build" "${cargo[@]}" "${e2e[@]}" --no-run
    step "fix: e2e_tui master picker rows" keyring "${cargo[@]}" "${e2e[@]}" --no-fail-fast
    step "fix: clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "fix: clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    step "fix: desktop clippy --lib" "${cargo[@]}" clippy -p crazyagent-desktop --lib --locked -- -D warnings
    known_red "fix: desktop clippy --tests (bridge.rs:1422 type_complexity = fix5)" "${cargo[@]}" clippy -p crazyagent-desktop --tests --locked --keep-going -- -D warnings
    git fetch -q --no-tags --depth=1 origin "$timeline" 2>&1 | tee -a "$out"
    at "$timeline"
    bins timeline
    step "timeline: crazyagent lib build" "${cargo[@]}" "${lib[@]}" --no-run
    for try in 1 2 3 4 5; do
      known_red "timeline: V244-TIMELINE-OWNER-HIDDEN-UBUNTU row alone, try $try" keyring "${cargo[@]}" "${lib[@]}" --no-fail-fast -E "$timeline_row"
    done
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
passed=$(grep -E '^[[:space:]]*PASS \[' "$out.lf" | awk '{print $NF}' | sort | uniq -c)
{
  cat "$table"
  echo
  echo "Failed rows, per name with count:"
  printf '%s\n' "$failed"
  echo
  echo "Passed rows, per name with count:"
  printf '%s\n' "$passed"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"
exit "$fail"
