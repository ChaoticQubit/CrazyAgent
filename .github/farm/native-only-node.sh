set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/native-only"
mkdir -p "$out"
failed=0
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| node run | expected | result |\n| --- | --- | --- |\n' >> "$summary"
paths=(apps/desktop/src apps/desktop/test)
red() {
  local sha="$1" want="$2" label="$3"
  git checkout "$sha" -- "${paths[@]}" || { row "red tree $sha" "check out" FAILED; exit 1; }
  (cd apps/desktop && node --test test/native-panel.test.mjs) > "$out/red-$sha.txt" 2>&1
  local rc=$? fails broken
  fails=$(grep -E '^# fail ' "$out/red-$sha.txt" | awk '{print $3}')
  broken=$(grep -cE 'SyntaxError|is not exported|Cannot find module|Failed to resolve|Could not resolve' "$out/red-$sha.txt")
  if [ "$rc" -ne 0 ] && [ "${fails:-0}" = "$want" ] && [ "$broken" -eq 0 ]; then
    row "$label at $sha" "$want fail at their assertions" "$fails fail, no load error"
  else
    row "$label at $sha" "$want fail at their assertions" "FAILED: rc $rc, fail ${fails:-none}, load errors $broken"; failed=1
  fi
  grep -E '^not ok' "$out/red-$sha.txt"
  cat "$out/red-$sha.txt"
  git checkout HEAD -- "${paths[@]}"
  [ -z "$(git status --porcelain)" ] || { row "tree after red $sha" "clean" "DIRTY"; git status --porcelain; exit 1; }
}
red 50397a821 14 "the 14 first native panel rows"
red 7ddb8db6a 2 "the prompt-after-Open-again and one-end rows, on the fcadf7e8c panel"
grep -E '^not ok' "$out/red-7ddb8db6a.txt" | grep -qE 'message box sends again' && grep -E '^not ok' "$out/red-7ddb8db6a.txt" | grep -qE 'counts as one end' \
  || { row "the two failing rows at 7ddb8db6a" "the prompt and one-end rows" "FAILED: other rows"; failed=1; }
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
suite web-panel test/kept-views.test.mjs test/kept-terminals.test.mjs test/one-terminal.test.mjs test/terminal-copy.test.mjs test/terminal-ended.test.mjs test/terminal-follow.test.mjs test/terminal-wall.test.mjs test/terminal-renderer.test.mjs test/terminal-renderer-policy.test.mjs test/terminal-scroll.test.mjs test/terminal-wheel.test.mjs test/terminal-input.test.mjs test/terminal-history.test.mjs test/terminal-queries.test.mjs
suite settings-and-console test/settings-access-layout.test.mjs test/live-master.test.mjs test/command-registration.test.mjs test/ide-tab.test.mjs
(cd apps/desktop && node --test test/command-wiring.test.mjs) > "$out/wiring.txt" 2>&1
missing=$(grep -oE 'desktop_[a-z_]+ (missing from|has no tauri command)' "$out/wiring.txt" | awk '{print $1}' | sort -u | tr '\n' ' ')
other=$(grep -oE 'desktop_[a-z_]+ (missing from|has no tauri command|is registered but never called)' "$out/wiring.txt" | grep -vc '^desktop_native_terminal_')
wfails=$(grep -E '^# fail ' "$out/wiring.txt" | awk '{print $3}')
row "command-wiring at the tip (red until feat/v243-native-wiring)" "1 fail, its first missing command desktop_native_terminal_*" "fail ${wfails:-none}; first missing: ${missing:-none}; others: $other"
[ "$other" -eq 0 ] && [ "${wfails:-0}" = 1 ] && [ -n "$missing" ] || failed=1
cat "$out/wiring.txt"
if (cd apps/desktop && npm run typecheck) > "$out/typecheck.txt" 2>&1; then row "typecheck at the tip" "pass" "pass"; else row "typecheck at the tip" "pass" "FAILED"; failed=1; fi
cat "$out/typecheck.txt"
exit "$failed"
