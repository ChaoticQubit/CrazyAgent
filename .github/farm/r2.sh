#!/bin/bash
set -uo pipefail
mode=$1
label=$2
commit=$3
shift 3
out="$RUNNER_TEMP/$label"
mkdir -p "$out"
git checkout -q "$commit" || { echo "::error::cannot check out $commit"; exit 1; }
echo "$mode at $(git rev-parse --short=9 HEAD)"
if [ -n "${OVERLAY:-}" ]; then
  git checkout -q "${OVERLAY%%:*}" -- "${OVERLAY#*:}" || { echo "::error::cannot overlay $OVERLAY"; exit 1; }
  echo "overlay ${OVERLAY#*:} from ${OVERLAY%%:*}"
fi
keyring() {
  dbus-run-session -- bash -uo pipefail -c 'printf "%s\n" ci-keyring | gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null; exec "$@"' farm "$@"
}
if [ "$mode" = green ]; then
  cargo clippy -p crazyagent --lib --locked -- -D warnings > "$out/clippy-lib.txt" 2>&1 || { grep -E '^(error|warning)' -A12 "$out/clippy-lib.txt" | head -150; echo "::error::clippy on the lib"; exit 1; }
  echo "CLIPPY lib clean"
  cargo clippy -p crazyagent --lib --profile test --locked -- -D warnings > "$out/clippy-test.txt" 2>&1 || { grep -E '^(error|warning)' -A12 "$out/clippy-test.txt" | head -150; echo "::error::clippy on the lib test build"; exit 1; }
  echo "CLIPPY lib test clean"
fi
cargo build -p crazyagent --bin crazyagent-landlock --locked > "$out/helper.txt" 2>&1 || { tail -20 "$out/helper.txt"; echo "::error::landlock helper build failed"; exit 1; }
mkdir -p target/debug/deps
cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock
cargo build -p crazyagent --bin crazyagent --locked > "$out/custodian.txt" 2>&1 || { tail -20 "$out/custodian.txt"; echo "::error::custodian build failed"; exit 1; }
export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$PWD/target/debug/crazyagent"
cargo test -p crazyagent --lib --locked --no-run > "$out/build.txt" 2>&1 || { grep -E '^(error|warning)' -A8 "$out/build.txt" | head -80; echo "::error::lib test build failed"; exit 1; }
bad=0
watched=engine::owner::transport::barrier_tests::a_revocation_waits_for_a_plugin_change_in_flight_and_refuses_the_next
suites="storage:: engine::owner::native_auth:: engine::owner::transport::barrier_tests:: engine::owner::transport::usage_tests:: engine::owner::transport::recovery_tests:: engine::owner::transport::native_auth engine::owner::transport::master_runtime:: engine::owner::transport::accounts engine::bootstrap engine::native_auth_host::"
run_suites() {
  local watched_red=0 run
  for run in $(seq 1 "$1"); do
    keyring cargo test -p crazyagent --lib --locked -- $suites --skip native_auth_registers_the_installed_claude_code_executable > "$out/suites-$run.txt" 2>&1
    echo "SUITES run $run: $(grep -E '^test result' "$out/suites-$run.txt" | tail -1)"
    grep -qE "^test $watched \.\.\. (ok|FAILED)$" "$out/suites-$run.txt" || { echo "::error::the watched row did not run (run $run)"; bad=1; }
    grep -E '^test .* FAILED$' "$out/suites-$run.txt" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sort -u > "$out/suite-failed-$run.txt"
    if grep -qxF "$watched" "$out/suite-failed-$run.txt"; then watched_red=$((watched_red + 1)); echo "WATCHED red: ${watched##*::} (run $run)"; fi
    grep -vxF "$watched" "$out/suite-failed-$run.txt" > "$out/suite-other-$run.txt"
    echo "SUITE OTHER REDS run $run: $(wc -l < "$out/suite-other-$run.txt")"
    cat "$out/suite-other-$run.txt"
    while read -r row; do grep -hE "^---- $row stdout ----" -A14 "$out/suites-$run.txt" | grep -E 'panicked at|left:|right:|assertion' | head -6; done < "$out/suite-other-$run.txt"
    [ ! -s "$out/suite-other-$run.txt" ] || bad=1
  done
  echo "WATCHED ${watched##*::}: red $watched_red of $1 at $(git rev-parse --short=9 HEAD)"
}
if [ "$mode" = suites ]; then
  run_suites "${SUITE_RUNS:-5}"
  exit "$bad"
fi
run_row() {
  local row=$1 try=$2 short=${1##*::}
  keyring cargo test -p crazyagent --lib --locked -- --exact "$row" > "$out/$short-$try.txt" 2>&1
  local code=$?
  grep -qE "^test $row \.\.\. (ok|FAILED)$" "$out/$short-$try.txt" || { echo "::error::$short did not run (try $try)"; tail -30 "$out/$short-$try.txt"; return 2; }
  if [ "$code" -eq 0 ]; then return 0; fi
  grep -hE 'panicked at' -A6 "$out/$short-$try.txt" | head -8
  return 1
}
if [ "$mode" = red ]; then
  for row in "$@"; do
    for i in 1 2 3; do
      run_row "$row" "$i"
      case $? in
        1) echo "RED: ${row##*::} (try $i)" ;;
        0) echo "::error::${row##*::} passed at the red commit (try $i)"; bad=1 ;;
        *) bad=1 ;;
      esac
    done
  done
  exit "$bad"
fi
keyring cargo test -p crazyagent --lib --locked -- storage::narrowing:: > "$out/store.txt" 2>&1 || { grep -hE 'panicked at' -A6 "$out/store.txt" | head -40; echo "::error::store rows failed"; bad=1; }
echo "STORE $(grep -E '^test result' "$out/store.txt" | tail -1)"
for row in "$@"; do
  run_row "$row" 1
  case $? in
    0) echo "GREEN: ${row##*::} (try 1)" ;;
    1) echo "::error::${row##*::} failed at the fix (try 1)"; bad=1 ;;
    *) bad=1 ;;
  esac
done
[ "$bad" -eq 0 ] || { echo "::error::a row failed its first try, so the 3x and the suites are skipped"; exit 1; }
for i in 2 3; do
  for row in "$@"; do
    run_row "$row" "$i"
    case $? in
      0) echo "GREEN: ${row##*::} (try $i)" ;;
      1) echo "::error::${row##*::} failed at the fix (try $i)"; bad=1 ;;
      *) bad=1 ;;
    esac
  done
done
run_suites "${SUITE_RUNS:-5}"
exit "$bad"
