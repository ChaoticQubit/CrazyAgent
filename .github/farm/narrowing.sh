#!/bin/bash
set -uo pipefail
out="$RUNNER_TEMP/narrowing"
mkdir -p "$out"
mode=${NARROWING_MODE:-red}
t=engine::owner::transport::barrier_tests
rows="$t::a_revocation_that_never_gets_the_owner_barrier_is_pending_and_lands_while_the_engine_runs $t::a_revocation_the_full_queue_refuses_is_pending_and_lands_while_the_engine_runs $t::a_revocation_pending_when_the_engine_dies_lands_at_the_next_open $t::a_revocation_refused_at_the_barrier_lands_at_the_next_open_when_the_engine_dies_mid_drain"
declare -A why=(
  [a_revocation_that_never_gets_the_owner_barrier_is_pending_and_lands_while_the_engine_runs]=owner_control_not_submitted
  [a_revocation_the_full_queue_refuses_is_pending_and_lands_while_the_engine_runs]=owner_queue_not_submitted
  [a_revocation_pending_when_the_engine_dies_lands_at_the_next_open]="the next open applies the revocation the dead engine never ran"
  [a_revocation_refused_at_the_barrier_lands_at_the_next_open_when_the_engine_dies_mid_drain]=owner_control_not_submitted
)
suites="engine::owner::transport::barrier_tests:: engine::owner::transport::usage_tests:: storage::"
bad=0
echo "mode $mode at $(git rev-parse --short=9 HEAD)"
cargo build -p crazyagent --bin crazyagent-landlock --locked > "$out/helper.txt" 2>&1 || { tail -20 "$out/helper.txt"; echo "::error::landlock helper build failed"; exit 1; }
mkdir -p target/debug/deps
cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock
cargo test -p crazyagent --lib --locked --no-run > "$out/build.txt" 2>&1 || { grep -E '^(error|warning)' -A8 "$out/build.txt" | head -80; echo "::error::lib test build failed"; exit 1; }
tail -2 "$out/build.txt"
for row in $rows; do
  short=${row##*::}
  for i in 1 2 3; do
    cargo test -p crazyagent --lib --locked -- --exact "$row" > "$out/$short-$i.txt" 2>&1
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
cargo test -p crazyagent --lib --locked -- $suites > "$out/suites.txt" 2>&1
echo "SUITES $(grep -E '^test result' "$out/suites.txt" | tail -1)"
grep -E '^test .* FAILED$' "$out/suites.txt" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sort -u > "$out/suite-failed.txt"
for row in $rows; do sed -i "\#^$row\$#d" "$out/suite-failed.txt"; done
echo "OTHER SUITE REDS $(wc -l < "$out/suite-failed.txt")"
cat "$out/suite-failed.txt"
[ ! -s "$out/suite-failed.txt" ] || { echo "::error::suite rows other than the new ones failed"; bad=1; }
cargo clippy -p crazyagent --lib --profile test --locked -- -D warnings > "$out/clippy.txt" 2>&1 || { grep -E '^(error|warning)' -A8 "$out/clippy.txt" | head -60; echo "::error::clippy on the lib test build"; bad=1; }
exit "$bad"
