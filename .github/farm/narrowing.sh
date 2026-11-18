#!/bin/bash
set -uo pipefail
out="$RUNNER_TEMP/narrowing"
mkdir -p "$out"
b=engine::owner::transport::barrier_tests
r=engine::owner::transport::native_auth_restart_tests
rows="$b::a_revocation_that_never_gets_the_owner_barrier_is_pending_and_lands_while_the_engine_runs $b::a_revocation_the_full_queue_refuses_is_pending_and_lands_while_the_engine_runs $b::a_revocation_pending_when_the_engine_dies_lands_at_the_next_open $b::a_revocation_refused_at_the_barrier_lands_at_the_next_open_when_the_engine_dies_mid_drain $r::a_sign_out_pending_when_the_engine_dies_signs_the_profile_out_at_the_next_start $r::a_leftover_sign_out_the_harness_cannot_finish_keeps_the_profile_blocked_and_says_why"
suites="storage:: engine::owner::transport::barrier_tests:: engine::owner::transport::usage_tests:: engine::owner::transport::native_auth_restart_tests:: engine::owner::transport::master_runtime:: engine::owner::transport::accounts engine::owner::transport::native_auth_channel"
bad=0
echo "green at $(git rev-parse --short=9 HEAD)"
keyring() {
  dbus-run-session -- bash -uo pipefail -c 'printf "%s\n" ci-keyring | gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null; exec "$@"' farm "$@"
}
cargo clippy -p crazyagent --lib --locked -- -D warnings > "$out/clippy-lib.txt" 2>&1 || { grep -E '^(error|warning)' -A12 "$out/clippy-lib.txt" | head -150; echo "::error::clippy on the lib"; exit 1; }
echo "CLIPPY lib clean"
cargo clippy -p crazyagent --lib --profile test --locked -- -D warnings > "$out/clippy-test.txt" 2>&1 || { grep -E '^(error|warning)' -A12 "$out/clippy-test.txt" | head -150; echo "::error::clippy on the lib test build"; exit 1; }
echo "CLIPPY lib test clean"
cargo build -p crazyagent --bin crazyagent-landlock --locked > "$out/helper.txt" 2>&1 || { tail -20 "$out/helper.txt"; echo "::error::landlock helper build failed"; exit 1; }
mkdir -p target/debug/deps
cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock
cargo build -p crazyagent --bin crazyagent --locked > "$out/custodian.txt" 2>&1 || { tail -20 "$out/custodian.txt"; echo "::error::custodian build failed"; exit 1; }
export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$PWD/target/debug/crazyagent"
cargo test -p crazyagent --lib --locked --no-run > "$out/build.txt" 2>&1 || { grep -E '^(error|warning)' -A8 "$out/build.txt" | head -80; echo "::error::lib test build failed"; exit 1; }
keyring cargo test -p crazyagent --lib --locked -- storage::narrowing:: > "$out/store.txt" 2>&1 || { grep -hE 'panicked at' -A6 "$out/store.txt" | head -40; echo "::error::store rows failed"; bad=1; }
echo "STORE $(grep -E '^test result' "$out/store.txt" | tail -1)"
for row in $rows; do
  short=${row##*::}
  for i in 1 2 3; do
    keyring cargo test -p crazyagent --lib --locked -- --exact "$row" > "$out/$short-$i.txt" 2>&1
    code=$?
    grep -qE "^test $row \.\.\. (ok|FAILED)$" "$out/$short-$i.txt" || { echo "::error::$short did not run (try $i)"; tail -30 "$out/$short-$i.txt"; bad=1; continue; }
    if [ "$code" -eq 0 ]; then echo "GREEN: $short (try $i)"; else grep -hE 'panicked at' -A8 "$out/$short-$i.txt" | head -12; echo "::error::$short failed at the fix (try $i)"; bad=1; fi
  done
done
keyring cargo test -p crazyagent --lib --locked -- $suites > "$out/suites.txt" 2>&1
echo "SUITES $(grep -E '^test result' "$out/suites.txt" | tail -1)"
grep -E '^test .* FAILED$' "$out/suites.txt" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sort -u > "$out/suite-failed.txt"
echo "SUITE REDS $(wc -l < "$out/suite-failed.txt")"
cat "$out/suite-failed.txt"
while read -r row; do grep -hE "^---- $row stdout ----" -A14 "$out/suites.txt" | grep -E 'panicked at|left:|right:|assertion' | head -6; done < "$out/suite-failed.txt"
[ ! -s "$out/suite-failed.txt" ] || { echo "::error::suite rows failed"; bad=1; }
exit "$bad"
