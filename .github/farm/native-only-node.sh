set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/native-only"
mkdir -p "$out"
failed=0
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| node run | expected | result |\n| --- | --- | --- |\n' >> "$summary"
red=50397a821
paths=(apps/desktop/src apps/desktop/test)
git checkout "$red" -- "${paths[@]}" || { row "red tree" "check out" FAILED; exit 1; }
(cd apps/desktop && node --test test/native-panel.test.mjs) > "$out/red.txt" 2>&1
rc=$?
fails=$(grep -E '^# fail ' "$out/red.txt" | awk '{print $3}')
broken=$(grep -cE 'SyntaxError|is not exported|Cannot find module|Failed to resolve|Could not resolve' "$out/red.txt")
if [ "$rc" -ne 0 ] && [ "${fails:-0}" = 14 ] && [ "$broken" -eq 0 ]; then
  row "14 native panel rows at $red" "all fail at their assertions" "$fails fail, no load error"
else
  row "14 native panel rows at $red" "all fail at their assertions" "FAILED: rc $rc, fail ${fails:-none}, load errors $broken"; failed=1
fi
cat "$out/red.txt"
git checkout HEAD -- "${paths[@]}"
[ -z "$(git status --porcelain)" ] || { row "tree after red" "clean" "DIRTY"; git status --porcelain; exit 1; }
suite() {
  local name="$1"; shift
  if (cd apps/desktop && node --test "$@") > "$out/$name.txt" 2>&1; then
    row "$name at the tip" "pass" "$(grep -E '^# (pass|fail)' "$out/$name.txt" | tr '\n' ' ')"
  else
    row "$name at the tip" "pass" "FAILED: $(grep -E '^# (pass|fail)' "$out/$name.txt" | tr '\n' ' ')"; failed=1
  fi
  cat "$out/$name.txt"
}
suite native-panel test/native-panel.test.mjs
suite web-panel test/kept-views.test.mjs test/kept-terminals.test.mjs test/one-terminal.test.mjs test/terminal-copy.test.mjs test/terminal-ended.test.mjs test/terminal-follow.test.mjs test/terminal-wall.test.mjs test/terminal-renderer.test.mjs test/terminal-renderer-policy.test.mjs
suite settings-and-console test/settings-access-layout.test.mjs test/live-master.test.mjs test/command-registration.test.mjs test/ide-tab.test.mjs
(cd apps/desktop && node --test test/command-wiring.test.mjs) > "$out/wiring.txt" 2>&1
missing=$(grep -oE 'desktop_[a-z_]+ (missing from|has no tauri command)' "$out/wiring.txt" | awk '{print $1}' | sort -u | tr '\n' ' ')
other=$(grep -oE 'desktop_[a-z_]+ (missing from|has no tauri command|is registered but never called)' "$out/wiring.txt" | grep -vc '^desktop_native_terminal_')
row "command-wiring at the tip (red until feat/v243-native-wiring)" "only desktop_native_terminal_* missing" "missing: ${missing:-none}; others: $other"
[ "$other" -eq 0 ] || failed=1
cat "$out/wiring.txt"
if (cd apps/desktop && npm run typecheck) > "$out/typecheck.txt" 2>&1; then row "typecheck at the tip" "pass" "pass"; else row "typecheck at the tip" "pass" "FAILED"; failed=1; fi
cat "$out/typecheck.txt"
exit "$failed"
