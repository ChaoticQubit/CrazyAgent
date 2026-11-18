set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/native-only"
mkdir -p "$out"
failed=0
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| %s run | expected | result |\n| --- | --- | --- |\n' "$RUNNER_OS" >> "$summary"
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
cargo build -p crazyagent-desktop --bin crazyagent-desktop --locked > "$out/desktop-bin.txt" 2>&1 || { row "desktop binary" "builds" "FAILED"; failed=1; cat "$out/desktop-bin.txt"; }
step smoke-desktop-links cargo test -p crazyagent --test smoke --locked -- --exact the_desktop_app_links_and_registers_every_command_its_build_lists
bin=target/debug/crazyagent-desktop
names='desktop_terminal_(attach|input|resize|detach)'
echo "== strings in $bin naming the four webview commands, with 3 strings of context"
strings -a -t x "$bin" > "$out/strings.txt" 2>/dev/null || strings -a -o "$bin" > "$out/strings.txt"
grep -nE "$names" "$out/strings.txt" | head -20
for line in $(grep -nE "$names" "$out/strings.txt" | head -8 | cut -d: -f1); do
  echo "-- string line $line"
  sed -n "$((line > 3 ? line - 3 : 1)),$((line + 3))p" "$out/strings.txt" | cut -c1-300
done
hits=$(grep -cE "$names" "$out/strings.txt")
echo "== the same names in the built frontend bundle"
grep -lE "$names" apps/desktop/dist/assets/*.js
echo "== the binary's sections with file offsets, to place each hit"
size -m -l -x "$bin" | grep -E 'Segment|Section' | head -80
row "macOS binary strings naming the four (diagnostic)" "listed" "$hits lines"
exit "$failed"
