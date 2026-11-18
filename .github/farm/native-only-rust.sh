set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/native-only"
mkdir -p "$out"
failed=0
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| %s run | expected | result |\n| --- | --- | --- |\n' "$RUNNER_OS" >> "$summary"
red=6308335dd
paths=(apps/desktop/src-tauri crates/crazyagent/tests/smoke.rs apps/desktop/test)
lib_row=tests::a_macos_build_answers_no_webview_terminal_attach_input_resize_or_detach
perm_row=the_webview_terminal_commands_are_built_and_granted_only_off_macos
git checkout "$red" -- "${paths[@]}" && rm -f apps/desktop/src-tauri/capabilities/webview-terminal.json || { row "red tree" "check out" FAILED; exit 1; }
if [ "$RUNNER_OS" = macOS ]; then
  if cargo test -p crazyagent-desktop --lib --locked -- --exact "$lib_row" > "$out/red-lib.txt" 2>&1; then
    row "lib red row at $red" "fails" "PASSED"; failed=1
  elif grep -q 'desktop_terminal_attach' "$out/red-lib.txt"; then
    row "lib red row at $red" "fails at its assertion" "fails at its assertion"
  else
    row "lib red row at $red" "fails at its assertion" "FAILED ELSEWHERE"; failed=1
  fi
  cat "$out/red-lib.txt"
fi
if cargo test -p crazyagent-desktop --test command_permissions --locked -- --exact "$perm_row" > "$out/red-perm.txt" 2>&1; then
  row "permission red row at $red" "fails" "PASSED"; failed=1
elif grep -q 'webview-terminal.json' "$out/red-perm.txt"; then
  row "permission red row at $red" "fails reading webview-terminal.json" "fails reading webview-terminal.json"
else
  row "permission red row at $red" "fails reading webview-terminal.json" "FAILED ELSEWHERE"; failed=1
fi
cat "$out/red-perm.txt"
git checkout HEAD -- "${paths[@]}"
[ -z "$(git status --porcelain)" ] || { row "tree after red" "clean" "DIRTY"; git status --porcelain; exit 1; }
step() {
  local name="$1"; shift
  if "$@" > "$out/$name.txt" 2>&1; then
    row "$name at the tip" "pass" "$(grep -E '^test result' "$out/$name.txt" | tail -1)"
  else
    row "$name at the tip" "pass" "FAILED"; failed=1
  fi
  cat "$out/$name.txt"
}
step desktop-lib cargo test -p crazyagent-desktop --lib --locked
step command-permissions cargo test -p crazyagent-desktop --test command_permissions --locked
step terminal-bridge-lists cargo test -p crazyagent-desktop --test terminal_bridge --locked -- graph_and_terminal_commands_are_registered_in_all_four_places wall_windows_reach_only_the_terminal_graph_stop_and_setting_read_commands
step projects-bridge-lists cargo test -p crazyagent-desktop --test projects_bridge --locked -- project_creation_has_one_named_native_command_and_local_main_window_permission
cargo build -p crazyagent-desktop --bin crazyagent-desktop --locked > "$out/desktop-bin.txt" 2>&1 || { row "desktop binary" "builds" "FAILED"; failed=1; cat "$out/desktop-bin.txt"; }
step smoke-desktop-links cargo test -p crazyagent --test smoke --locked -- --exact the_desktop_app_links_and_registers_every_command_its_build_lists
cargo clippy -p crazyagent-desktop --all-targets --locked --message-format short -- -D warnings > "$out/clippy.txt" 2>&1
rc=$?
mine=$(grep -cE '^apps/desktop/src-tauri/(src/lib\.rs|build\.rs|tests/(command_permissions|terminal_bridge|projects_bridge)\.rs):[0-9]+:[0-9]+: (error|warning)' "$out/clippy.txt")
row "desktop clippy --all-targets -D warnings" "nothing in the changed files" "exit $rc, $mine in the changed files"
[ "$mine" -eq 0 ] || failed=1
cat "$out/clippy.txt"
exit "$failed"
