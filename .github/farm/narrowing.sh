#!/bin/bash
set -uo pipefail
out="$RUNNER_TEMP/narrowing"
mkdir -p "$out"
mode=${NARROWING_MODE:-red}
t=engine::owner::transport::native_auth_restart_tests
rows="$t::a_sign_out_pending_when_the_engine_dies_signs_the_profile_out_at_the_next_start $t::a_leftover_sign_out_the_harness_cannot_finish_keeps_the_profile_blocked_and_says_why"
declare -A why=(
  [a_sign_out_pending_when_the_engine_dies_signs_the_profile_out_at_the_next_start]="the leftover sign-out is lost: the profile launches on its old sign-in"
  [a_leftover_sign_out_the_harness_cannot_finish_keeps_the_profile_blocked_and_says_why]="a failed harness sign-out leaves the profile blocked"
)
suite="$t::"
bad=0
echo "mode $mode at $(git rev-parse --short=9 HEAD)"
keyring() {
  dbus-run-session -- bash -uo pipefail -c 'printf "%s\n" ci-keyring | gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null; exec "$@"' farm "$@"
}
cargo build -p crazyagent --bin crazyagent-landlock --locked > "$out/helper.txt" 2>&1 || { tail -20 "$out/helper.txt"; echo "::error::landlock helper build failed"; exit 1; }
mkdir -p target/debug/deps
cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock
cargo build -p crazyagent --bin crazyagent --locked > "$out/custodian.txt" 2>&1 || { tail -20 "$out/custodian.txt"; echo "::error::custodian build failed"; exit 1; }
export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$PWD/target/debug/crazyagent"
cargo test -p crazyagent --lib --locked --no-run > "$out/build.txt" 2>&1 || { grep -E '^(error|warning)' -A8 "$out/build.txt" | head -80; echo "::error::lib test build failed"; exit 1; }
tail -2 "$out/build.txt"
for row in $rows; do
  short=${row##*::}
  for i in 1 2 3; do
    keyring cargo test -p crazyagent --lib --locked -- --exact "$row" > "$out/$short-$i.txt" 2>&1
    code=$?
    grep -qE "^test $row \.\.\. (ok|FAILED)$" "$out/$short-$i.txt" || { echo "::error::$short did not run (try $i)"; tail -30 "$out/$short-$i.txt"; bad=1; continue; }
    grep -hE 'panicked at' -A6 "$out/$short-$i.txt" | head -8
    if [ "$mode" = red ]; then
      if [ "$code" -eq 0 ]; then
        echo "::error::$short passed before the fix (try $i)"; bad=1
      elif grep -qF "${why[$short]}" "$out/$short-$i.txt"; then
        echo "RED for the named reason: $short (try $i)"
      else
        echo "::error::$short failed for another reason than '${why[$short]}' (try $i)"; bad=1
      fi
    else
      [ "$code" -eq 0 ] && echo "GREEN: $short (try $i)" || { echo "::error::$short failed at the fix (try $i)"; bad=1; }
    fi
  done
done
keyring cargo test -p crazyagent --lib --locked -- "$suite" > "$out/suite.txt" 2>&1
echo "SUITES $(grep -E '^test result' "$out/suite.txt" | tail -1)"
grep -E '^test .* FAILED$' "$out/suite.txt" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sort -u > "$out/suite-failed.txt"
for row in $rows; do sed -i "\#^$row\$#d" "$out/suite-failed.txt"; done
echo "OTHER SUITE REDS $(wc -l < "$out/suite-failed.txt")"
cat "$out/suite-failed.txt"
[ ! -s "$out/suite-failed.txt" ] || { echo "::error::suite rows other than the new ones failed"; bad=1; }
exit "$bad"
