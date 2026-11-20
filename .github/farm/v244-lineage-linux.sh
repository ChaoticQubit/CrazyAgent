set -uo pipefail
out="$RUNNER_TEMP/v244-lineage-linux.txt"
tip=c8a3f6666
base=238ab31c7
linux_rows="agents::tests::a_log_holder_outside_the_agents_group_is_signalled_only_when_it_carries_the_launch_token agents::tests::a_stop_names_each_log_holder_it_left_by_id_and_program_and_counts_the_rest agents::tests::an_agent_register_from_before_lineage_gets_the_lineage_and_stop_left_columns agents::tests::only_processes_whose_environment_shows_the_launch_token_are_carriers"
mac_rows="agents::tests::stopping_an_agent_never_ends_the_owners_own_writer_of_its_log agents::tests::stopping_an_agent_ends_an_escaped_log_holder_only_when_it_can_tell_it_is_the_agents agents::tests::stopping_a_reattached_agent_ends_its_escaped_child_only_when_it_can_tell_it_is_the_agents agents::tests::stopping_a_reattached_agent_never_signals_a_foreign_group agents::tests::a_launched_agent_keeps_its_launch_token_with_its_run"
filters="agents:: harnesses::leftovers::"
failed=0
exe=""
: > "$out"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" | tee -a "$out"
}
code_tip=$(git log -1 --format=%h --abbrev=9 HEAD -- . ':!.github')
if [ "$code_tip" != "$tip" ]; then
  row "code tip" "$tip" "NOT: $code_tip"
  exit 1
fi
row "code tip" "$tip" "$code_tip"
farm_head=$(git rev-parse HEAD)
row "host" "linux" "$(uname -srm)"
label_of() {
  printf '%s' "$1" | sed 's/::/__/g'
}
block() {
  awk -v head="---- $1 stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$2" | grep -A4 'panicked at' | head -6 | cut -c1-300
}
reds() {
  grep -E '^test .* FAILED$' "$1" | awk '{print $2}' | sort | tr '\n' ' ' || true
}
at() {
  local commit=$1 label=$2
  git checkout -q -f --detach "$commit" || { row "checkout $commit" ok NOT; failed=1; return 1; }
  cargo build -p crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/helper-$label-linux.txt" 2>&1 || { row "landlock helper at $commit" built NOT; tail -20 "$RUNNER_TEMP/helper-$label-linux.txt" | tee -a "$out"; failed=1; return 1; }
  exe=$(cargo test -p crazyagent --lib --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-$label-linux.txt" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))$/\1/p' | head -1)
  if [ -z "$exe" ] || [ ! -x "$exe" ]; then
    row "lib test binary at $commit" built NOT
    grep -E -A8 '^error' "$RUNNER_TEMP/build-$label-linux.txt" | head -40 | tee -a "$out"
    failed=1
    return 1
  fi
  exe=$(realpath "$exe")
  row "lib test binary at $commit" built built
}
filter_run() {
  local commit=$1 label=$2 f=$3
  local log="$RUNNER_TEMP/filter-$(label_of "$f")-$label-linux.txt"
  "$exe" "$f" > "$log" 2>&1
  local listed
  listed=$("$exe" "$f" --list 2>/dev/null | grep -c ': test$' || true)
  local result
  result=$(grep -E '^test result:' "$log" | tail -1)
  row "$f at $commit, rows listed" ">0" "$listed"
  row "$f at $commit" "result" "$result"
  if [ "${listed:-0}" -eq 0 ]; then
    failed=1
  fi
}
at "$tip" tip || exit 1
for f in $filters; do
  filter_run "$tip" tip "$f"
done
agents_log="$RUNNER_TEMP/filter-agents__-tip-linux.txt"
for r in $linux_rows; do
  if grep -qxF "test $r ... ok" "$agents_log"; then
    row "$r at $tip" ok ok
  else
    row "$r at $tip" ok "NOT: $(grep -F "test $r " "$agents_log" | head -1)"
    block "$r" "$agents_log" | tee -a "$out"
    failed=1
  fi
done
for r in $mac_rows; do
  if grep -qF "test $r " "$agents_log"; then
    row "$r at $tip" "not built on Linux" "NOT: ran"
    failed=1
  else
    row "$r at $tip" "not built on Linux" "not built on Linux"
  fi
done
tip_reds=""
for f in $filters; do
  tip_reds="$tip_reds $(reds "$RUNNER_TEMP/filter-$(label_of "$f")-tip-linux.txt")"
done
tip_reds=$(printf '%s\n' $tip_reds | sed '/^$/d' | sort -u | tr '\n' ' ')
row "reds at $tip" "listed" "${tip_reds:-none}"
for r in $tip_reds; do
  for f in $filters; do
    block "$r" "$RUNNER_TEMP/filter-$(label_of "$f")-tip-linux.txt" | tee -a "$out"
  done
done
if [ -n "${tip_reds// /}" ]; then
  at "$base" base || exit 1
  base_reds=""
  for f in $filters; do
    filter_run "$base" base "$f"
    base_reds="$base_reds $(reds "$RUNNER_TEMP/filter-$(label_of "$f")-base-linux.txt")"
  done
  base_reds=$(printf '%s\n' $base_reds | sed '/^$/d' | sort -u | tr '\n' ' ')
  row "reds at $base" "listed" "${base_reds:-none}"
  new=""
  for r in $tip_reds; do
    case " $base_reds " in
      *" $r "*) ;;
      *) new="$new $r" ;;
    esac
  done
  if [ -n "${new// /}" ]; then
    row "new reds at $tip" none "NOT:$new"
    failed=1
  else
    row "new reds at $tip" none none
  fi
fi
git checkout -q -f --detach "$farm_head"
for target in --lib --tests; do
  log="$RUNNER_TEMP/clippy$target-linux.txt"
  cargo clippy -p crazyagent "$target" --locked -- -D warnings > "$log" 2>&1
  rc=$?
  if [ "$rc" -eq 0 ]; then row "clippy -p crazyagent $target -D warnings at $tip" "rc 0" "rc 0"; else
    row "clippy -p crazyagent $target -D warnings at $tip" "rc 0" "NOT: rc $rc"
    grep -E -A10 '^(warning|error)' "$log" | head -80 | cut -c1-300 | tee -a "$out"
    failed=1
  fi
done
echo "lineage linux bad=$failed" | tee -a "$out"
exit "$failed"
