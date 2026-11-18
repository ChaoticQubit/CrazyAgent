set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
tip=6af8dcb26
fix=d7f4160d0
red=f144afb7b
base=dd648a01a
lib_rows="sandbox::linux::tests::a_nest_takes_exact_read_only_empty_folder_pairs_before_its_working_folder engine::owner::transport::master_host_launch::tests::master_sandbox_hides_a_listed_folder_from_a_real_child_on_linux"
lib_kept="sandbox::linux::tests::a_nest_takes_only_its_network_and_working_folder_before_the_command sandbox::linux::tests::bubblewrap_starts_the_agent_in_a_new_session engine::owner::transport::master_host_launch::tests::a_tools_off_master_reads_the_repository_but_cannot_move_its_refs"
box_rows="a_hidden_folder_is_out_of_reach_on_both_bubblewrap_backends a_link_the_agent_makes_to_a_hidden_folder_reads_only_the_empty_folder an_absent_hidden_folder_cannot_be_made_on_both_bubblewrap_backends a_hidden_folder_in_a_read_root_is_out_of_reach_on_both_bubblewrap_backends landlock_alone_refuses_a_hidden_folder_and_names_what_makes_it_work an_installed_sandbox_with_a_hidden_folder_keeps_it_out_or_refuses_it a_confined_agent_with_a_hidden_folder_is_refused_as_confined_not_for_the_folder"
box_kept="bubblewrap_holds_the_same_project_boundary an_installed_sandbox_on_linux_launches_through_the_linux_boundary"
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
git log --oneline -5
code_tip=$(git log -1 --format=%H HEAD -- . ':!.github')
if [ "$(git rev-parse --short=9 "$code_tip")" != "$tip" ] || [ "$(git rev-parse --short=9 "$code_tip~1")" != "$fix" ] || [ "$(git rev-parse --short=9 "$code_tip~2")" != "$red" ] || [ "$(git rev-parse --short=9 "$code_tip~3")" != "$base" ]; then
  row tip "$tip on $fix on $red on $base" "NOT: $(git log --format=%h --abbrev=9 -4 "$code_tip" | tr '\n' ' ')"
  exit 1
fi
bwrap=$(command -v bwrap)
"$bwrap" --version
if "$bwrap" --die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts --dev-bind / / --proc /proc --unshare-net -- /bin/true; then
  row "this runner's bubblewrap nests (the nested rows run, not skip)" yes yes
else
  row "this runner's bubblewrap nests (the nested rows run, not skip)" yes "NOT: no"
  failed=1
fi
block() {
  local name=$1 file=$2
  awk -v head="---- $name stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$file" | head -40
}
judge() {
  local label=$1 file=$2 name=$3 want=$4
  local got
  if grep -qF -- "test $name ... ok" "$file"; then
    got=passes
  elif grep -qF -- "test $name ... FAILED" "$file"; then
    got=fails
  else
    got="not run"
    tail -20 "$file"
  fi
  echo "== $name $label: $got (expected $want)"
  [ "$got" = fails ] && block "$name" "$file"
  if [ "$got" = "$want" ]; then
    row "$name $label" "$want" "$got"
  else
    row "$name $label" "$want" "NOT: $got $(block "$name" "$file" | grep -m1 -A3 'panicked at' | tail -3 | tr '\n' ' ' | cut -c1-400)"
    failed=1
  fi
}
failures() {
  local file=$1
  for name in $(grep -E '^test .* FAILED$' "$file" | awk '{print $2}'); do
    echo "-- $name"
    block "$name" "$file" | grep -A4 'panicked at' | head -6
  done
}
echo "== the rows at $tip (red at $red and base $base linux_sandbox proven in run 44380858756)"
git checkout -q "$code_tip"
cargo clippy -p crazyagent --all-targets --locked -- -D warnings > "$RUNNER_TEMP/clippy.txt" 2>&1
code=$?
tail -3 "$RUNNER_TEMP/clippy.txt"
if [ "$code" -eq 0 ]; then
  row "clippy -p crazyagent --all-targets -D warnings at $tip" clean clean
else
  grep -E -A8 '^(error|warning)' "$RUNNER_TEMP/clippy.txt" | head -80 || true
  row "clippy -p crazyagent --all-targets -D warnings at $tip" clean "NOT: exit $code"
  failed=1
fi
cargo test -p crazyagent --lib --locked -- sandbox:: master_host_launch > "$RUNNER_TEMP/unit.txt" 2>&1
grep -E '^test result' "$RUNNER_TEMP/unit.txt" || true
for name in $lib_rows $lib_kept; do
  judge "at $tip" "$RUNNER_TEMP/unit.txt" "$name" passes
done
failures "$RUNNER_TEMP/unit.txt"
row "lib sandbox:: master_host_launch at $tip" "record" "$(grep -E '^test result' "$RUNNER_TEMP/unit.txt" | tail -1); failed: $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/unit.txt" | awk '{print $2}' | tr '\n' ' ')"
cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -2
echo "== linux_sandbox in full at $tip"
cargo test -p crazyagent --test linux_sandbox --locked --no-fail-fast > "$RUNNER_TEMP/full.txt" 2>&1
grep -E '^test result' "$RUNNER_TEMP/full.txt" || true
for name in $box_rows $box_kept; do
  judge "at $tip" "$RUNNER_TEMP/full.txt" "$name" passes
done
failures "$RUNNER_TEMP/full.txt"
new=$(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}' | tr '\n' ' ')
row "linux_sandbox at $tip" "record" "$(grep -E '^test result' "$RUNNER_TEMP/full.txt" | tail -1)"
if [ -z "$new" ]; then
  row "linux_sandbox reds at $tip (none at $base in run 44380858756)" none none
else
  row "linux_sandbox reds at $tip (none at $base in run 44380858756)" none "NOT: $new"
  failed=1
fi
exit "$failed"
