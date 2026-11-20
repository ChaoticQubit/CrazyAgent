set -uo pipefail
tip=7f7b7c7fa
base=f6d054bd4
os=${FARM_OS:?FARM_OS is windows or macos}
out="$RUNNER_TEMP/w3-4b"
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
NEW_BOOT="engine::bootstrap::anchor_windows_tests::a_windows_engine_start_reaches_start_inner_and_serves engine::bootstrap::anchor_windows_tests::a_windows_start_with_settings_sources_reaches_start_inner_and_serves engine::bootstrap::anchor_windows_tests::a_locator_endpoint_with_a_zero_part_is_refused engine::bootstrap::anchor_windows_tests::a_started_windows_engine_serves_an_owner_client_and_stops_clean engine::bootstrap::anchor_windows_tests::a_second_windows_start_on_a_served_anchor_is_refused_at_the_database_lock engine::bootstrap::anchor_windows_tests::a_replaced_generation_is_refused_before_the_endpoint_check engine::bootstrap::anchor_windows_tests::a_locator_changed_in_place_or_renamed_over_while_held_fails_verify"
NEW_IPC="ipc::windows_tests::a_publish_over_a_live_listeners_pipes_file_fails_until_it_closes ipc::windows_tests::a_listener_holds_its_root_and_pipes_file_and_removes_the_file_at_drop ipc::windows_tests::a_publish_gives_up_on_a_pipes_file_held_through_every_try"
NEW_OWNER="engine::owner::transport::owner_pipe_windows_tests::after_the_owner_server_stops_no_pipes_or_capability_file_is_left"
NEW_STORE="a_private_file_is_deleted_through_a_handle_with_delete_access remove_deletes_a_private_file_only_while_it_holds_its_pinned_identity"
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
      lib_rows "ipc:: engine::windows_tests:: engine::bootstrap:: engine::owner::transport::owner_pipe_windows_tests:: engine::owner::transport::root_windows_tests:: engine::owner::transport::key_windows_tests:: service::"
      need "$out/rows.log" $NEW_BOOT $NEW_IPC $NEW_OWNER
    else
      bad=1
    fi
    ;;
  *) echo "bad FARM_OS $os"; exit 2 ;;
esac
classify
[ "$bad" -eq 0 ] && say "W3-4b $os ok at $tip"
exit "$bad"
