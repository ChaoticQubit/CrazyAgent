set -uo pipefail
tip=cd3b0b8f6
base=238ab31c7
os=${FARM_OS:?FARM_OS is windows or macos}
out="$RUNNER_TEMP/wire2"
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
NEW_IPC="ipc::windows_tests::an_owner_endpoint_at_a_path_serves_a_client_of_this_user ipc::windows_tests::the_halves_of_a_pipe_stream_run_both_ways_at_once ipc::windows_tests::a_listener_refuses_the_client_past_its_cap_with_pipe_busy ipc::windows_tests::a_bind_over_a_pipes_file_of_another_endpoint_is_refused ipc::windows_tests::a_listener_holds_its_root_and_pipes_file_and_removes_the_file_at_drop ipc::windows_tests::answering_is_true_only_while_a_checked_listener_serves ipc::windows_tests::a_server_the_pipes_file_does_not_pin_is_refused"
NEW_PURE="ipc::endpoint_tests::an_endpoint_path_names_its_root_and_its_file_name_as_the_purpose ipc::endpoint_tests::an_endpoint_path_outside_the_plain_shape_is_refused ipc::endpoint_tests::a_listener_of_n_streams_makes_n_plus_one_instances ipc::endpoint_tests::a_stream_count_outside_the_instance_cap_is_a_programming_error"
NEW_ENGINE="engine::windows_tests::an_ordinary_request_round_trips_over_the_engine_pipe engine::windows_tests::a_bind_in_a_root_that_is_not_private_is_refused engine::windows_tests::a_second_engine_on_a_served_root_is_refused_with_engine_pipe_taken engine::windows_tests::a_rogue_instance_with_another_pin_gets_no_byte_from_a_request engine::windows_tests::a_restricted_client_is_refused_before_any_frame_is_parsed engine::windows_tests::after_run_stops_the_pipes_file_is_gone_and_the_endpoint_does_not_answer engine::windows_tests::a_medium_client_of_an_elevated_engine_gets_no_reply"
NEW_STORE="a_private_file_is_deleted_through_a_handle_with_delete_access remove_deletes_a_private_file_only_while_it_holds_its_pinned_identity remove_leaves_a_held_file_and_a_hard_linked_one"
clippy() {
  local pkg kind log rc
  for pkg in crazyagent crazyagent-win-sandbox; do
    for kind in --lib --tests; do
      log="$out/clippy-$pkg$kind.log"
      cargo clippy -p "$pkg" "$kind" --locked $([ "$pkg" = crazyagent-win-sandbox ] && echo --features test-principals) -- -D warnings > "$log" 2>&1
      rc=$?
      say "clippy $pkg $kind rc=$rc, $(grep -cE '^(warning|error)' "$log") warning/error lines"
      [ "$rc" -eq 0 ] || { grep -E -A8 '^(warning|error)' "$log" | head -80 | cut -c1-300; bad=1; }
    done
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
    n=$(grep -cE "^${p}[^ ]*: test$" "$out/list.txt")
    say "rows $p: $n"
    [ "$n" -gt 0 ] || { say "ZERO ROWS $p"; bad=1; }
    grep -E "^${p}[^ ]*: test$" "$out/list.txt" | sed 's/: test$//' >> "$names"
  done
  sort -u -o "$names" "$names"
  cargo test -p crazyagent --lib --locked -- $prefixes > "$log" 2>&1
  rc=$?
  say "lib rows rc=$rc | $(grep '^test result' "$log" | tail -1 | cut -c1-120)"
  grep -E '^test .* \.\.\. FAILED$' "$log" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' > "$out/failed.txt"
  grep -E '^test .* \.\.\. ok$' "$log" | sed -E 's/^test (.*) \.\.\. ok$/\1/' > "$out/passed.txt"
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
classify() {
  [ -s "$out/failed.txt" ] || return 0
  say "classify: the tip's failing rows at the base $base"
  git checkout -q -f --detach "$base" || return 0
  cargo test -p crazyagent --lib --locked -- --list > "$out/list-base.txt" 2>/dev/null || { say "base list did not build"; return 0; }
  grep -xFf <(sed 's/$/: test/' "$out/failed.txt") "$out/list-base.txt" | sed 's/: test$//' > "$out/old.txt"
  if [ -s "$out/old.txt" ]; then
    cargo test -p crazyagent --lib --locked -- --exact $(cat "$out/old.txt") > "$out/base.log" 2>&1
    grep -E '^test .* \.\.\. (ok|FAILED)$' "$out/base.log" | sed 's/^/base: /'
  fi
  grep -vxFf "$out/old.txt" "$out/failed.txt" | sed 's/^/new at tip, not at base: /'
  git checkout -q -f --detach "$tip"
}
case "$os" in
  windows)
    clippy
    cargo build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked > "$out/bins.log" 2>&1 || { tail -30 "$out/bins.log" | cut -c1-300; exit 1; }
    mkdir -p target/debug/deps
    cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe
    for t in private_windows pipe_windows peer_judge; do
      log="$out/store-$t.log"
      cargo test -p crazyagent-win-sandbox --test "$t" --locked > "$log" 2>&1
      rc=$?
      say "win-sandbox --test $t rc=$rc | $(grep '^test result' "$log" | tail -1 | cut -c1-120)"
      grep -E '^test .* \.\.\. FAILED$' "$log" | head -20
      grep -A8 'panicked at' "$log" | head -60 | cut -c1-400
      [ "$rc" -eq 0 ] || bad=1
    done
    winwho="$(cygpath -u "$SYSTEMROOT")/System32/whoami.exe"
    say "runner integrity: $(MSYS_NO_PATHCONV=1 "$winwho" /groups | grep -i 'Mandatory Label' | tr -s ' ' | cut -c1-120)"
    cargo check -p crazyagent-win-sandbox --release --features test-principals --locked > "$out/guard.log" 2>&1
    rc=$?
    hit=$(grep -c 'never reaches a release build' "$out/guard.log" || true)
    say "guard release+test-principals rc=$rc message lines $hit (want rc!=0, 1+)"
    { [ "$rc" -ne 0 ] && [ "$hit" -ge 1 ]; } || bad=1
    prod=$(cargo tree -p crazyagent --locked -e normal,build,features -i crazyagent-win-sandbox 2>&1 | grep -c 'test-principals' || true)
    dev=$(cargo tree -p crazyagent --locked -e dev,features -i crazyagent-win-sandbox 2>&1 | grep -c 'test-principals' || true)
    say "tree test-principals lines: normal+build $prod (want 0), dev $dev (want 1+)"
    { [ "$prod" -eq 0 ] && [ "$dev" -ge 1 ]; } || bad=1
    need "$out/store-private_windows.log" $NEW_STORE
    need "$out/store-peer_judge.log" a_peer_below_the_engines_own_integrity_is_refused
    if listed; then
      lib_rows "ipc:: service:: engine::windows_tests::"
      need "$out/rows.log" $NEW_IPC $NEW_PURE $NEW_ENGINE
    else
      bad=1
    fi
    ;;
  macos)
    cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock --locked > "$out/bins.log" 2>&1 || { tail -30 "$out/bins.log" | cut -c1-300; exit 1; }
    mkdir -p target/debug/deps
    cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock
    export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$PWD/target/debug/crazyagent"
    listed || exit 1
    lib_rows "ipc:: engine::tests:: engine::owner::transport:: service::"
    need "$out/rows.log" $NEW_PURE ipc::tests::an_owner_endpoint_serves_a_client_of_this_user_at_its_path
    ;;
  *) echo "bad FARM_OS $os"; exit 2 ;;
esac
classify
[ "$bad" -eq 0 ] && say "WIRE-2 $os ok at $tip"
exit "$bad"
