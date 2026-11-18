set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/native-only"
mkdir -p "$out"
failed=0
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| node run | expected | result |\n| --- | --- | --- |\n' >> "$summary"
red=03f058fdd
paths=(apps/desktop/src apps/desktop/test)
git checkout "$red" -- "${paths[@]}" || { row "red tree" "check out" FAILED; exit 1; }
if (cd apps/desktop && node --test --test-name-pattern 'V243-NATIVE-ONLY' test/settings-access-layout.test.mjs) > "$out/red.txt" 2>&1; then
  row "settings red row at $red" "fails" "PASSED"; failed=1
elif grep -q 'Terminal renderer' "$out/red.txt"; then
  row "settings red row at $red" "fails at its assertion" "fails at its assertion"
else
  row "settings red row at $red" "fails at its assertion" "FAILED ELSEWHERE"; failed=1
fi
cat "$out/red.txt"
git checkout HEAD -- "${paths[@]}"
[ -z "$(git status --porcelain)" ] || { row "tree after red" "clean" "DIRTY"; exit 1; }
if (cd apps/desktop && node --test test/settings-access-layout.test.mjs test/command-registration.test.mjs test/command-wiring.test.mjs test/ide-tab.test.mjs test/terminal-renderer-policy.test.mjs test/terminal-renderer.test.mjs) > "$out/green.txt" 2>&1; then
  row "settings, command and renderer suites at the tip" "pass" "$(grep -E '^# (pass|fail)' "$out/green.txt" | tr '\n' ' ')"
else
  row "settings, command and renderer suites at the tip" "pass" "FAILED: $(grep -E '^# (pass|fail)' "$out/green.txt" | tr '\n' ' ')"; failed=1
fi
cat "$out/green.txt"
if (cd apps/desktop && npm run typecheck) > "$out/typecheck.txt" 2>&1; then row "typecheck at the tip" "pass" "pass"; else row "typecheck at the tip" "pass" "FAILED"; failed=1; fi
cat "$out/typecheck.txt"
exit "$failed"
