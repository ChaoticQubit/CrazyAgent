set -uo pipefail
tip=f2617e7c4
base=238ab31c7
out="$RUNNER_TEMP/guard-lineage"
mkdir -p "$out"
bad=0
short() { git rev-parse --short=9 "$1"; }
say() { printf '%s\n' "$*"; }
code=$(git log -1 --format=%H HEAD -- . ':!.github')
if [ "$(short "$code")" != "$tip" ] || ! git merge-base --is-ancestor "$base" "$tip"; then
  say "chain NOT: code $(short "$code"), want $tip on $base"
  exit 1
fi
farm_head=$(git rev-parse HEAD)
say "tree $(short "$code") on $base; $(git --version); runner ${ImageOS:-unknown} ${ImageVersion:-unknown}; $(sw_vers -productVersion)"
CRAZYAGENT_FARM_PROBE=seen /usr/bin/python3 -I -c 'import sys, time; print(sys.executable, flush=True); time.sleep(4)' > "$out/python.txt" &
probe=$!
sleep 2
shown=$(/bin/ps -E -ww -o command= -p "$probe" 2>/dev/null | grep -o 'CRAZYAGENT_FARM_PROBE=seen' | head -1)
wait "$probe"
say "fixture python $(head -1 "$out/python.txt"): environment ${shown:+shown}${shown:-HIDDEN}"
GROUP_ROW=harnesses::launch::tests::a_stop_names_no_group_at_or_below_one_nor_past_a_process_id
HOLDER_ROW=agents::tests::a_log_holder_stop_never_signals_process_or_group_one
LINEAGE_RED="stopping_an_agent_ends_its_escaped_child_but_never_the_owners_own_writer_of_its_log
stopping_an_agent_leaves_a_log_holder_whose_environment_is_hidden_and_names_it
stopping_a_reattached_agent_never_signals_a_foreign_group_nor_a_hidden_holder_and_names_it
a_log_holder_outside_the_agents_group_is_signalled_only_when_it_carries_the_launch_token
a_stop_names_each_log_holder_it_left_by_id_and_program_and_counts_the_rest
an_agent_register_from_before_lineage_gets_the_lineage_and_stop_left_columns
a_launched_agent_keeps_its_launch_token_with_its_run
only_processes_whose_environment_shows_the_launch_token_are_carriers"
LINEAGE_GUARD=stopping_a_reattached_agent_ends_the_escaped_child_that_carries_its_launch_token
exe=""
at() {
  git checkout -q -f --detach "$1" || { say "checkout $1 NOT"; bad=1; return 1; }
  say "== at $(short HEAD) $(date -u +%H:%M:%S)"
  exe=$(cargo test -p crazyagent --lib --locked --no-run 2>&1 | tee "$out/build-$2.log" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))$/\1/p' | head -1)
  if [ -z "$exe" ] || [ ! -x "$exe" ]; then
    say "build at $1 NOT"
    grep -E -A8 '^error' "$out/build-$2.log" | head -40 | cut -c1-300
    bad=1
    return 1
  fi
  exe=$(realpath "$exe")
}
one() {
  local row=$1 want=$2 label=$3 rc passed got
  (cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" "$exe" --exact "$row" > "$out/$label.log" 2>&1)
  rc=$?
  passed=$(grep -cE "^test $row \.\.\. ok$" "$out/$label.log")
  got=red
  [ "$rc" -eq 0 ] && [ "$passed" -eq 1 ] && got=green
  if [ "$got" = "$want" ]; then say "| $label | $want | $got |"; else say "| $label | $want | NOT: $got |"; bad=1; fi
  if [ "$got" = red ]; then grep -A3 'panicked at' "$out/$label.log" | head -4 | cut -c1-300; fi
}
filter() {
  local name=$1 label=$2 rc
  (cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" "$exe" "$name" > "$out/$label.log" 2>&1)
  rc=$?
  say "$label rc=$rc $(grep -E '^test result:' "$out/$label.log" | tail -1)"
  grep -E '^test .* FAILED$' "$out/$label.log" | head -20
  grep -A3 'panicked at' "$out/$label.log" | head -24 | cut -c1-300
  if [ "$rc" -ne 0 ] || ! grep -qE '^test result: ok\. [1-9]' "$out/$label.log"; then bad=1; fi
}
if at 299fb9b89 group-red; then one "$GROUP_ROW" red group-row-at-299fb9b89; fi
if at 6796e6d6a holder-red; then
  one "$GROUP_ROW" green group-row-at-6796e6d6a
  one "$HOLDER_ROW" red holder-row-at-6796e6d6a
fi
if at 0ea8e8c1a guard-tip; then
  one "$GROUP_ROW" green group-row-at-0ea8e8c1a
  one "$HOLDER_ROW" green holder-row-at-0ea8e8c1a
fi
if at bf992ee7b lineage-red; then
  for row in $LINEAGE_RED; do one "agents::tests::$row" red "$row-at-bf992ee7b"; done
  one "agents::tests::$LINEAGE_GUARD" green "$LINEAGE_GUARD-at-bf992ee7b"
fi
if at "$tip" tip; then
  for row in $LINEAGE_RED $LINEAGE_GUARD; do one "agents::tests::$row" green "$row-at-$tip"; done
  one "$GROUP_ROW" green group-row-at-tip
  one "$HOLDER_ROW" green holder-row-at-tip
  filter harnesses::launch:: launch-at-tip
  filter harnesses::terminal_session:: terminal-session-at-tip
  filter agents:: agents-at-tip
  filter harnesses::leftovers:: leftovers-at-tip
fi
for kind in --lib --tests; do
  log="$out/clippy$kind.log"
  cargo clippy -p crazyagent "$kind" --locked -- -D warnings > "$log" 2>&1
  rc=$?
  say "clippy crazyagent $kind at $tip rc=$rc, $(grep -cE '^(warning|error)' "$log") warning/error lines"
  if [ "$rc" -ne 0 ]; then grep -E -A8 '^(warning|error)' "$log" | head -80 | cut -c1-300; bad=1; fi
done
git checkout -q -f --detach "$farm_head"
say "guard-lineage bad=$bad"
exit "$bad"
