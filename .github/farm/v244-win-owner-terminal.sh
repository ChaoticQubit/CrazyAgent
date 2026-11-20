set -uo pipefail
green=70c408a98
fix=6cf73d67a
red=a10bbe781
red_crates=ea7b888b9bb20e53c5d29d65e7e12b3367c34b34
green_crates=af6fd447f444dfe500c7858b0f9fe798a2a3eaef
os=${FARM_OS:?FARM_OS is windows or linux}
out="$RUNNER_TEMP/w4"
mkdir -p "$out"
bad=0
say() { printf '%s\n' "$*"; }
short() { git rev-parse --short=9 "$1"; }
[ "$(git rev-parse HEAD:crates)" = "$green_crates" ] || { say "chain NOT: crates $(git rev-parse HEAD:crates), want $green_crates"; exit 1; }
git merge-base --is-ancestor "$green" HEAD || { say "chain NOT: $green is not under HEAD"; exit 1; }
say "tree $(short HEAD) green $green fix $red..$fix; runner ${ImageOS:-unknown} ${ImageVersion:-unknown}"
W4="engine::owner::transport::terminal::windows_tests::"
T12="an_owner_shell_attaches_over_the_owner_pipe_with_its_size a_pane_attach_of_an_unknown_pane_gets_the_engines_refusal a_rogue_owner_instance_gets_no_terminal_packet an_owner_shell_view_types_resizes_scrolls_and_detaches a_changed_grant_ends_an_owner_shell_view"
T3="an_input_only_view_of_an_exited_shell_ends_as_terminal_ended a_planted_git_bat_in_the_project_folder_does_not_run a_paste_without_bracketed_paste_reaches_the_shell_as_typed_keys the_host_reads_bracketed_paste_on_only_after_the_app_turns_it_on"
ROWS="$T12 $T3"
clippy() {
  local kind log rc
  for kind in --lib --tests; do
    log="$out/clippy$kind.log"
    cargo clippy -p crazyagent "$kind" --locked -- -D warnings > "$log" 2>&1
    rc=$?
    say "clippy crazyagent $kind rc=$rc, $(grep -cE '^(warning|error)' "$log") warning/error lines"
    [ "$rc" -eq 0 ] || { grep -E -A8 '^(warning|error)' "$log" | head -80 | cut -c1-300; bad=1; }
  done
}
w4_rows() {
  local leg=$1 dir=$2 log="$out/$1.log" row
  (cd "$dir" && CARGO_TARGET_DIR="$GITHUB_WORKSPACE/target" cargo test -p crazyagent --lib --locked -- "$W4" --test-threads=1 --show-output) > "$log" 2>&1
  say "$leg rows rc=$? | $(grep '^test result' "$log" | tail -1 | cut -c1-120)"
  grep -E '^(w4 row:|error\[|error:)' "$log" | head -20 | cut -c1-300
  for row in $ROWS; do
    if grep -qE "^test ${W4}${row} \.\.\. ok$" "$log"; then say "$leg ok $row";
    elif grep -qE "^test ${W4}${row} \.\.\. FAILED$" "$log"; then say "$leg FAILED $row";
    else say "$leg MISSING $row"; fi
  done
  grep -A6 'panicked at' "$log" | head -80 | cut -c1-400
}
case "$os" in
  windows)
    git worktree add -q --detach "$RUNNER_TEMP/red" HEAD || exit 1
    git diff "$red" "$fix" | git -C "$RUNNER_TEMP/red" apply -R || { say "red: reverse apply of $red..$fix failed"; exit 1; }
    git -C "$RUNNER_TEMP/red" add -A crates
    got=$(git -C "$RUNNER_TEMP/red" write-tree --prefix=crates/)
    [ "$got" = "$red_crates" ] || { say "red crates $got, want $red_crates"; exit 1; }
    say "red tree = $red (T3 rows, no T3 fix) + W3-3 (crates $got)"
    w4_rows red "$RUNNER_TEMP/red"
    for row in $T3; do
      grep -qE "^test ${W4}${row} \.\.\. FAILED$" "$out/red.log" || { say "RED NOT FAILED: $row"; bad=1; }
    done
    for row in $T12; do
      grep -qE "^test ${W4}${row} \.\.\. ok$" "$out/red.log" || { say "T1 T2 ROW NOT OK AT RED: $row"; bad=1; }
    done
    git worktree remove --force "$RUNNER_TEMP/red" > /dev/null 2>&1 || true
    clippy
    w4_rows green "$PWD"
    for row in $ROWS; do
      grep -qE "^test ${W4}${row} \.\.\. ok$" "$out/green.log" || { say "GREEN NOT OK: $row"; bad=1; }
    done
    log="$out/owner-pipe.log"
    cargo test -p crazyagent --lib --locked -- engine::owner::transport::owner_pipe_windows_tests:: terminal:: > "$log" 2>&1
    rc=$?
    say "owner pipe + terminal:: rows rc=$rc | $(grep '^test result' "$log" | tail -1 | cut -c1-120)"
    grep -E '^test .* \.\.\. FAILED$' "$log" | head -20
    grep -A6 'panicked at' "$log" | head -40 | cut -c1-400
    [ "$rc" -eq 0 ] || bad=1
    ;;
  linux)
    clippy
    ;;
  *) echo "bad FARM_OS $os"; exit 2 ;;
esac
[ "$bad" -eq 0 ] && say "W4 T1 T2 T3 $os ok at $green"
exit "$bad"
