set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/native-input"
mkdir -p "$out"
failed=0
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| node run | expected | result |\n| --- | --- | --- |\n' >> "$summary"
base=58be7bbf2
nodetest() { (cd apps/desktop && node --test --test-reporter=tap "$@"); }
expect() {
  local file="$1" want="$2" label="$3" name
  shift 3
  nodetest "test/$file" > "$out/$label.txt" 2>&1
  local rc=$? fails broken
  fails=$(grep -E '^# fail ' "$out/$label.txt" | awk '{print $3}')
  broken=$(grep -cE 'SyntaxError|is not exported|Cannot find module|Failed to resolve|Could not resolve' "$out/$label.txt")
  if [ "$rc" -ne 0 ] && [ "${fails:-0}" = "$want" ] && [ "$broken" -eq 0 ]; then
    row "$label" "$want fail at their assertions" "$fails fail, no load error"
  else
    row "$label" "$want fail at their assertions" "FAILED: rc $rc, fail ${fails:-none}, load errors $broken"; failed=1
  fi
  for name in "$@"; do
    grep -E '^not ok' "$out/$label.txt" | grep -qF "$name" || { row "$label" "$name fails" "FAILED: it did not fail"; failed=1; }
  done
  grep -E '^not ok' "$out/$label.txt"
  cat "$out/$label.txt"
}
git checkout "$base" -- apps/desktop/src || { row "B panel $base" "check out" FAILED; exit 1; }
expect native-panel.test.mjs 9 "native-panel rows at the tip on the B panel $base" "V243-NATIVE-INPUT E1: with Terminal input null" "V243-NATIVE-INPUT E1: with Terminal input terminal" "V243-NATIVE-INPUT E1: with Terminal input box" "V243-NATIVE-INPUT E1: with Terminal input both" "V243-NATIVE-INPUT E2" "V243-NATIVE-INPUT E4: a native view gets the keyboard" "V243-NATIVE-INPUT E4: Jump to latest" "V243-NATIVE-INPUT E5" "V243-NATIVE-SEED-REFUSED"
expect terminal-ended.test.mjs 1 "terminal-ended at the tip on the B copy $base" "V243-NATIVE-INPUT E5"
git checkout HEAD -- apps/desktop/src
[ -z "$(git status --porcelain)" ] || { row "tree after red" "clean" "DIRTY"; git status --porcelain; exit 1; }
suite() {
  local name="$1"; shift
  if nodetest "$@" > "$out/$name.txt" 2>&1; then
    row "$name at the tip" "pass" "$(grep -E '^# (pass|fail) ' "$out/$name.txt" | tr '\n' ' ')"
  else
    row "$name at the tip" "pass" "FAILED: $(grep -E '^# (pass|fail) ' "$out/$name.txt" | tr '\n' ' ')"; failed=1
  fi
  grep -E '^not ok' "$out/$name.txt"
  cat "$out/$name.txt"
}
suite native-panel test/native-panel.test.mjs
suite web-panel test/kept-views.test.mjs test/kept-terminals.test.mjs test/one-terminal.test.mjs test/terminal-ended.test.mjs test/terminal-follow.test.mjs test/terminal-wall.test.mjs test/terminal-renderer.test.mjs test/terminal-renderer-policy.test.mjs test/terminal-scroll.test.mjs test/terminal-wheel.test.mjs test/terminal-input.test.mjs test/terminal-history.test.mjs test/terminal-queries.test.mjs
expect terminal-copy.test.mjs 1 "terminal-copy at the tip (Linux Shift drag, fixed by #490 a4a1e0a2a, not in B)" "copy works in the master and agent terminal of every harness kind"
suite settings-and-console test/settings-access-layout.test.mjs test/live-master.test.mjs test/command-registration.test.mjs test/ide-tab.test.mjs
nodetest test/command-wiring.test.mjs > "$out/wiring.txt" 2>&1
missing=$(grep -oE 'desktop_[a-z_]+ (missing from|has no tauri command)' "$out/wiring.txt" | awk '{print $1}' | sort -u | tr '\n' ' ')
other=$(grep -oE 'desktop_[a-z_]+ (missing from|has no tauri command|is registered but never called)' "$out/wiring.txt" | grep -vc '^desktop_native_terminal_')
wfails=$(grep -E '^# fail ' "$out/wiring.txt" | awk '{print $3}')
row "command-wiring at the tip (red until feat/v243-native-wiring)" "1 fail, its first missing command desktop_native_terminal_*" "fail ${wfails:-none}; first missing: ${missing:-none}; others: $other"
[ "$other" -eq 0 ] && [ "${wfails:-0}" = 1 ] && [ -n "$missing" ] || failed=1
cat "$out/wiring.txt"
if (cd apps/desktop && npm run typecheck) > "$out/typecheck.txt" 2>&1; then row "typecheck at the tip" "pass" "pass"; else row "typecheck at the tip" "pass" "FAILED"; failed=1; fi
cat "$out/typecheck.txt"
exit "$failed"
