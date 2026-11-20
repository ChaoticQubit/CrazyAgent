set -uo pipefail
tip=0cfbdb021
base=238ab31c7
red=bf992ee7b
out="$RUNNER_TEMP/guard-lineage"
mkdir -p "$out"
bad=0
exe=""
short() { git rev-parse --short=9 "$1"; }
say() { printf '%s\n' "$*"; }
code=$(git log -1 --format=%H HEAD -- . ':!.github')
if [ "$(short "$code")" != "$tip" ] || ! git merge-base --is-ancestor "$base" "$tip" || ! git merge-base --is-ancestor "$red" "$tip"; then
  say "chain NOT: code $(short "$code"), want $tip on $red on $base"
  exit 1
fi
farm_head=$(git rev-parse HEAD)
say "tree $(short "$code") on $base; runner ${ImageOS:-unknown} ${ImageVersion:-unknown}; macOS $(sw_vers -productVersion)"
py=$(/usr/bin/python3 -I -c 'import sys; print(sys.executable)')
probe() {
  local label=$1
  shift
  CRAZYAGENT_FARM_PROBE=seen "$@" > /dev/null 2>&1 &
  local pid=$!
  sleep 2
  local shown
  shown=$(/bin/ps -E -ww -o command= -p "$pid" 2>/dev/null | grep -o 'CRAZYAGENT_FARM_PROBE=seen' | head -1)
  say "| env probe | $label | ${shown:+shown}${shown:-hidden} | $(/bin/ps -o comm= -p "$pid" 2>/dev/null) |"
  kill "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
}
probe "/bin/sleep" /bin/sleep 6
probe "/bin/sleep under sandbox-exec allow default" /usr/bin/sandbox-exec -p '(version 1)(allow default)' /bin/sleep 6
probe "$py" "$py" -I -c 'import time; time.sleep(6)'
probe "$py under sandbox-exec allow default" /usr/bin/sandbox-exec -p '(version 1)(allow default)' "$py" -I -c 'import time; time.sleep(6)'
GROUP_ROW=harnesses::launch::tests::a_stop_names_no_group_at_or_below_one_nor_past_a_process_id
HOLDER_ROW=agents::tests::a_log_holder_stop_never_signals_process_or_group_one
LINEAGE="stopping_an_agent_never_ends_the_owners_own_writer_of_its_log
stopping_an_agent_ends_an_escaped_log_holder_only_when_it_can_tell_it_is_the_agents
stopping_a_reattached_agent_ends_its_escaped_child_only_when_it_can_tell_it_is_the_agents
stopping_a_reattached_agent_never_signals_a_foreign_group
a_log_holder_outside_the_agents_group_is_signalled_only_when_it_carries_the_launch_token
a_stop_names_each_log_holder_it_left_by_id_and_program_and_counts_the_rest
an_agent_register_from_before_lineage_gets_the_lineage_and_stop_left_columns
a_launched_agent_keeps_its_launch_token_with_its_run
only_processes_whose_environment_shows_the_launch_token_are_carriers"
FILTERS="harnesses::launch:: harnesses::terminal_session:: agents:: harnesses::leftovers::"
build() {
  local label=$1
  say "== $label at $(short HEAD) $(date -u +%H:%M:%S)"
  exe=$(cargo test -p crazyagent --lib --locked --no-run 2>&1 | tee "$out/build-$label.log" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))$/\1/p' | head -1)
  if [ -z "$exe" ] || [ ! -x "$exe" ]; then
    say "build $label NOT"
    grep -E -A8 '^error' "$out/build-$label.log" | head -40 | cut -c1-300
    bad=1
    return 1
  fi
  exe=$(realpath "$exe")
}
run() {
  local file=$1
  shift
  (cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" "$exe" "$@" > "$file" 2>&1)
}
one() {
  local row=$1 want=$2 label=$3 got
  run "$out/$label.log" --exact "$row"
  got=red
  grep -qE "^test $row \.\.\. ok$" "$out/$label.log" && got=green
  if [ "$got" = "$want" ]; then say "| $label | $want | $got |"; else say "| $label | $want | NOT: $got |"; bad=1; fi
  [ "$got" = red ] && grep -A3 'panicked at' "$out/$label.log" | head -4 | cut -c1-400
  return 0
}
reds_in() {
  grep -E '^test .* FAILED$' "$1" | awk '{print $2}' | sort -u
}
filters() {
  local label=$1 f name
  for f in $FILTERS; do
    name=$(echo "$f" | tr -c 'a-z_\n' '_')
    run "$out/filter-$name-$label.log" "$f"
    say "$f at $label: $(grep -E '^test result:' "$out/filter-$name-$label.log" | tail -1)"
    reds_in "$out/filter-$name-$label.log" >> "$out/reds-$label.txt"
  done
  sort -u -o "$out/reds-$label.txt" "$out/reds-$label.txt"
}
git checkout -q -f --detach "$base"
: > "$out/reds-base.txt"
if build base; then
  filters base
  say "reds at $base: $(tr '\n' ' ' < "$out/reds-base.txt")"
  while read -r name; do
    grep -A2 'panicked at' "$out"/filter-*-base.log | grep -A2 "'$name'" | head -3 | cut -c1-300
  done < "$out/reds-base.txt"
fi
git checkout -q -f --detach "$red"
git checkout -q "$tip" -- crates/crazyagent/src/agents/tests.rs
if build red-with-tip-rows; then
  for row in $LINEAGE; do one "agents::tests::$row" red "$row-at-$red"; done
fi
git checkout -q -f --detach "$farm_head"
: > "$out/reds-tip.txt"
if build tip; then
  for row in $LINEAGE; do one "agents::tests::$row" green "$row-at-$tip"; done
  one "$GROUP_ROW" green group-row-at-tip
  one "$HOLDER_ROW" green holder-row-at-tip
  filters tip
  say "reds at $tip: $(tr '\n' ' ' < "$out/reds-tip.txt")"
  for name in $(comm -13 "$out/reds-base.txt" "$out/reds-tip.txt"); do
    passes=0
    for try in 1 2 3; do
      run "$out/retry-$name-$try.log" --exact "$name"
      grep -qE "^test $name \.\.\. ok$" "$out/retry-$name-$try.log" && passes=$((passes + 1))
    done
    if [ "$passes" -gt 0 ]; then
      say "| $name | red at $tip only | flaky: $passes of 3 retries pass |"
    else
      say "| $name | red at $tip only | NOT: red on 3 retries |"
      grep -A3 'panicked at' "$out/retry-$name-1.log" | head -4 | cut -c1-400
      bad=1
    fi
  done
fi
for kind in --lib --tests; do
  log="$out/clippy$kind.log"
  cargo clippy -p crazyagent "$kind" --locked -- -D warnings > "$log" 2>&1
  rc=$?
  say "clippy crazyagent $kind at $tip rc=$rc"
  if [ "$rc" -ne 0 ]; then grep -E -A8 '^(warning|error)' "$log" | head -60 | cut -c1-300; bad=1; fi
done
say "guard-lineage r2 bad=$bad"
exit "$bad"
