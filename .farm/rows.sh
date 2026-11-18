set -uo pipefail
set +e
out="$RUNNER_TEMP/open-up-$PLATFORM-$SIDE.txt"
git log --oneline -1 | tee "$out"
git status --short | tee -a "$out"
cargo +"$RUST_TOOLCHAIN" test -p crazyagent --locked --lib --no-run 2>&1 | tail -3
read -ra modules <<< "$MODULES"
cargo +"$RUST_TOOLCHAIN" test -p crazyagent --locked --lib -- "${modules[@]}" 2>&1 | tee -a "$out"
ok=$(grep -cE '^test [a-z0-9_:]+ \.\.\. ok$' "$out")
failed=$(grep -E '^test [a-z0-9_:]+ \.\.\. FAILED$' "$out" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sort -u)
read -ra rows <<< "$NEW_ROWS"
verdict=pass
expected=""
for row in "${rows[@]}"; do
  got=$(grep -E "^test $row \.\.\. " "$out" | sed -E 's/.* \.\.\. //')
  if [ "$PLATFORM" = linux ]; then want=none; elif [ "$SIDE" = base ]; then want=FAILED; expected="$expected$row"$'\n'; else want=ok; fi
  echo "row $row: ${got:-none}, want $want"
  [ "${got:-none}" = "$want" ] || verdict=fail
done
others=$(comm -23 <(printf '%s\n' "$failed" | sed '/^$/d') <(printf '%s' "$expected" | sed '/^$/d' | sort -u))
[ -z "$others" ] || verdict=fail
[ "$ok" -gt 50 ] || verdict=fail
echo "other failed rows:"
printf '%s\n' "$others"
{ echo "| platform | side | ok | failed | other failed | verdict |"; echo "| --- | --- | --- | --- | --- | --- |"; echo "| $PLATFORM | $SIDE | $ok | $(printf '%s\n' "$failed" | sed '/^$/d' | wc -l | tr -d ' ') | $(printf '%s\n' "$others" | sed '/^$/d' | wc -l | tr -d ' ') | $verdict |"; } >> "$GITHUB_STEP_SUMMARY"
echo "VERDICT $PLATFORM $SIDE ok=$ok failed=$(printf '%s\n' "$failed" | sed '/^$/d' | wc -l | tr -d ' ') other_failed=$(printf '%s\n' "$others" | sed '/^$/d' | wc -l | tr -d ' ') verdict=$verdict"
[ "$verdict" = pass ]
