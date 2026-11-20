set -uo pipefail
green=a17a6625e
fix=ba412156a
red=6ccc69589
red_crates=89b7b4da5d8a8c866cd65b533ce8664fd0d84d9a
green_crates=5ea0dce4cc455c6904b2737e0608daef2d5f9192
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
ROWS="an_owner_shell_attaches_over_the_owner_pipe_with_its_size a_pane_attach_of_an_unknown_pane_gets_the_engines_refusal a_rogue_owner_instance_gets_no_terminal_packet an_owner_shell_view_types_resizes_scrolls_and_detaches a_changed_grant_ends_an_owner_shell_view"
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
    say "red tree = 6ccc69589 + W3-3 (crates $got)"
    w4_rows red "$RUNNER_TEMP/red"
    for row in $ROWS; do
      grep -qE "^test ${W4}${row} \.\.\. FAILED$" "$out/red.log" || { say "RED NOT FAILED: $row"; bad=1; }
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
[ "$bad" -eq 0 ] && say "W4 T1 T2 $os ok at $green"
exit "$bad"
