set -uo pipefail
tip=313698079
rows=6c1fce6a0
base=e3036f4a6
os=${FARM_OS:?FARM_OS is windows}
out="$RUNNER_TEMP/w5a"
mkdir -p "$out"
bad=0
short() { git rev-parse --short=9 "$1"; }
say() { printf '%s\n' "$*"; }
code=$(git log -1 --format=%H HEAD -- . ':!.github')
if [ "$(short "$code")" != "$tip" ] || ! git merge-base --is-ancestor "$base" "$tip"; then
  say "chain NOT: code $(short "$code"), want $tip on $base"
  exit 1
fi
git checkout -q -f --detach "$tip" || exit 1
say "tree $(short HEAD) on $base; git $(git --version); runner ${ImageOS:-unknown} ${ImageVersion:-unknown}"
NEW="landing::windows_tests::each_refused_landing_function_refuses_by_name_on_windows landing::windows_tests::a_windows_engine_git_step_or_call_is_refused_and_starts_nothing landing::windows_tests::the_master_landing_path_on_windows_records_nothing landing::windows_tests::the_store_reads_of_one_change_set_answer_on_windows landing::windows_tests::a_bounded_read_on_windows_refuses_a_link_and_reads_a_plain_file landing::windows_tests::a_windows_engine_read_names_no_work_tree_and_starts_in_the_git_folder landing::windows_tests::a_windows_engine_call_on_a_work_tree_without_a_step_is_refused_and_starts_nothing landing::windows_tests::a_windows_engine_read_keeps_the_engine_settings_spelled_for_windows_and_reads_no_outside_config landing::windows_tests::a_windows_engine_read_runs_the_one_judged_git landing::windows_tests::a_windows_repository_with_a_config_hook_or_an_include_is_refused_at_open landing::windows_tests::a_windows_work_tree_compare_or_untracked_listing_is_a_step_and_stays_refused landing::windows_tests::no_windows_agent_profile_writes_the_owner_git_folder_and_a_worktree_checkout_is_refused landing::windows_tests::a_windows_step_writes_only_its_step_folder_its_own_home_and_its_kind_target landing::windows_tests::the_three_windows_step_containers_differ_from_each_other_owner_git_and_agents landing::windows_tests::a_windows_worktree_made_again_at_the_same_path_gets_new_step_containers landing::windows_tests::a_windows_step_profile_that_is_a_link_or_names_another_account_is_refused landing::windows_tests::a_windows_work_tree_holding_a_junction_or_a_folder_symlink_is_refused_before_a_scan landing::windows_tests::a_windows_work_tree_walk_refuses_a_junction_root_and_stops_at_its_bound landing::windows_tests::a_windows_shadow_config_keeps_only_the_keys_that_shape_a_scan landing::windows_tests::a_windows_shadow_git_folder_holds_a_detached_head_and_the_allowlisted_config_only landing::windows_tests::no_windows_step_profile_guards_a_path_inside_the_work_tree landing::windows_tests::the_windows_no_excludes_file_sits_where_no_step_container_writes landing::windows_tests::a_windows_engine_read_of_attributes_or_grafts_answers_with_the_null_device"
clippy() {
  local kind log rc
  for kind in --lib --tests; do
    log="$out/clippy-crazyagent$kind.log"
    cargo clippy -p crazyagent "$kind" --locked --keep-going -- -D warnings > "$log" 2>&1
    rc=$?
    say "clippy crazyagent $kind rc=$rc, $(grep -cE '^(warning|error)' "$log" || true) warning/error lines"
    [ "$rc" -eq 0 ] || { grep -E -A7 '^(warning|error)' "$log" | grep -vE '^\s*\|?\s*$' | head -400 | cut -c1-220; bad=1; }
  done
}
listed() {
  local log="$out/list.txt"
  cargo test -p crazyagent --lib --locked -- --list > "$log" 2> "$out/list.err" || { tail -40 "$out/list.err" | cut -c1-300; return 1; }
  say "crazyagent lib rows: $(grep -c ': test$' "$log")"
}
lib_rows() {
  local prefixes=$1 log="$out/rows.log" p n names="$out/sel.txt" rc
  : > "$names"
  for p in $prefixes; do
    n=$(grep -cE "^${p}[^ ]*: test$" "$out/list.txt" || true)
    say "rows $p: $n"
    [ "$n" -gt 0 ] || { say "ZERO ROWS $p"; bad=1; }
    grep -E "^${p}[^ ]*: test$" "$out/list.txt" | sed 's/: test$//' >> "$names" || true
  done
  sort -u -o "$names" "$names"
  cargo test -p crazyagent --lib --locked -- $prefixes > "$log" 2>&1
  rc=$?
  say "lib rows rc=$rc | $(grep '^test result' "$log" | tail -1 | cut -c1-120)"
  grep -E '^test .* \.\.\. FAILED$' "$log" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' > "$out/failed.txt" || true
  grep -E '^test .* \.\.\. ok$' "$log" | sed -E 's/^test (.*) \.\.\. ok$/\1/' > "$out/passed.txt" || true
  say "lib: $(wc -l < "$out/passed.txt" | tr -d ' ') ok, $(wc -l < "$out/failed.txt" | tr -d ' ') FAILED of $(wc -l < "$names" | tr -d ' ') listed"
  sed 's/^/FAILED /' "$out/failed.txt" | head -40
  grep -A8 'panicked at' "$log" | head -80 | cut -c1-400
  [ "$rc" -eq 0 ] || bad=1
}
need() {
  local file=$1 row
  shift
  for row in "$@"; do
    grep -qE "^test ${row}( - should panic)? \.\.\. ok$" "$file" || { say "ROW NOT OK: $row"; bad=1; }
  done
}
red_at_base() {
  say "red: the W5b2 rows on the base with the W3-5b carry, $rows"
  git checkout -q -f --detach "$rows" || return 0
  if cargo test -p crazyagent --lib --locked -- --list > "$out/list-rows.txt" 2> "$out/list-rows.err"; then
    say "rows commit built: $(grep -c '^landing::windows_tests::' "$out/list-rows.txt" || true) rows listed (want no build)"
    bad=1
  else
    say "rows commit lib tests did not build (red, as wanted): $(grep -m3 -E '^error' "$out/list-rows.err" | cut -c1-160 | tr '\n' ' ')"
  fi
  git checkout -q -f --detach "$tip"
}
case "$os" in
  windows)
    clippy
    cargo build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked > "$out/helper.log" 2>&1 || { tail -30 "$out/helper.log" | cut -c1-300; bad=1; }
    say "helper built: $(test -f target/debug/crazyagent-win-boundary.exe && echo yes || echo no)"
    if listed; then
      lib_rows "landing::"
      need "$out/rows.log" $NEW
    else
      bad=1
    fi
    red_at_base
    ;;
  *) echo "bad FARM_OS $os"; exit 2 ;;
esac
[ "$bad" -eq 0 ] && say "W5b2 $os ok at $tip"
exit "$bad"
