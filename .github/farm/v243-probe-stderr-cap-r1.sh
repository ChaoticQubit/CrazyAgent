set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
tip=c4271bed2
red=8e0808864
first=432edc71e
rows="sandbox::linux::tests::a_failed_probe_is_read_only_to_its_cap_and_still_ends sandbox::linux::tests::a_failed_probe_is_logged_on_one_line_with_its_control_characters_escaped"
kept="sandbox::linux::tests::a_failed_probe_names_its_own_reason_and_only_an_enforcement_answer_names_the_kernel sandbox::linux::tests::no_agent_starts_without_the_helper_and_the_refusal_says_so"
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
git log --oneline -4
code_tip=$(git log -1 --format=%H HEAD -- . ':!.github')
if [ "$(git rev-parse --short=9 "$code_tip")" != "$tip" ] || [ "$(git rev-parse --short=9 "$code_tip~1")" != "$red" ] || [ "$(git rev-parse --short=9 "$code_tip~2")" != "$first" ]; then
  row tip "$tip on $red on $first" "NOT: $(git log --format=%h --abbrev=9 -3 "$code_tip" | tr '\n' ' ')"
  exit 1
fi
block() {
  local name=$1 file=$2
  awk -v head="---- $name stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$file" | head -30
}
judge() {
  local label=$1 file=$2 name=$3 want=$4
  local got
  if grep -qF -- "test $name ... ok" "$file"; then
    got=passes
  elif grep -qF -- "test $name ... FAILED" "$file"; then
    got=fails
    block "$name" "$file"
  else
    got="not run"
    tail -20 "$file"
  fi
  echo "== $name $label: $got (expected $want)"
  if [ "$got" = "$want" ]; then
    row "$name $label" "$want" "$got"
  else
    row "$name $label" "$want" "NOT: $got $(block "$name" "$file" | grep -m1 -A1 'panicked at' | tail -1 | cut -c1-160)"
    failed=1
  fi
}
echo "== red at $red"
git checkout -q "$red"
cargo test -p crazyagent --lib --locked --no-run 2>&1 | tail -2
cargo test -p crazyagent --lib --locked -- --exact $rows > "$RUNNER_TEMP/red.txt" 2>&1
for name in $rows; do
  judge "at $red" "$RUNNER_TEMP/red.txt" "$name" fails
  block "$name" "$RUNNER_TEMP/red.txt" | grep -A3 'panicked at' | head -4
done
echo "== the fix at $tip"
git checkout -q "$code_tip"
cargo clippy -p crazyagent --all-targets --locked -- -D warnings > "$RUNNER_TEMP/clippy.txt" 2>&1
code=$?
tail -3 "$RUNNER_TEMP/clippy.txt"
if [ "$code" -eq 0 ]; then
  row "clippy -p crazyagent --all-targets -D warnings at $tip" clean clean
else
  grep -E -A8 '^(error|warning)' "$RUNNER_TEMP/clippy.txt" | head -60 || true
  row "clippy -p crazyagent --all-targets -D warnings at $tip" clean "NOT: exit $code"
  failed=1
fi
cargo test -p crazyagent --lib --locked -- sandbox::linux > "$RUNNER_TEMP/unit.txt" 2>&1
grep -E '^test result|FAILED$' "$RUNNER_TEMP/unit.txt" || true
for name in $rows $kept; do
  judge "at $tip" "$RUNNER_TEMP/unit.txt" "$name" passes
done
for name in $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/unit.txt" | awk '{print $2}'); do
  echo "-- $name"
  block "$name" "$RUNNER_TEMP/unit.txt" | grep -A2 'panicked at' | head -4
done
row "lib sandbox::linux at $tip" "record" "$(grep -E '^test result' "$RUNNER_TEMP/unit.txt" | tail -1); failed: $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/unit.txt" | awk '{print $2}' | tr '\n' ' ')"
cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -2
echo "== linux_sandbox in full at $tip"
cargo test -p crazyagent --test linux_sandbox --locked --no-fail-fast > "$RUNNER_TEMP/full.txt" 2>&1
grep -E '^test .* FAILED$|^test result' "$RUNNER_TEMP/full.txt" || true
for name in $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}'); do
  echo "-- $name"
  block "$name" "$RUNNER_TEMP/full.txt" | grep -A2 'panicked at' | head -4
done
grep -h 'linux_helper_missing\|linux_probe_failed' "$RUNNER_TEMP/full.txt" | sort | uniq -c | head -6 || true
row "linux_sandbox at $tip" "record" "$(grep -E '^test result' "$RUNNER_TEMP/full.txt" | tail -1); failed: $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}' | tr '\n' ' ')"
exit "$failed"
