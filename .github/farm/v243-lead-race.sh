set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/race"
mkdir -p "$out"
base=44468d483ed2e3b4bae40264ab9d122a2788ddf1
tip=8ea4c6436c1e4d49d46489e22ac280e00466825f
file=crates/crazyagent/src/team/runtime/tests.rs
rows=(
  team::runtime::tests::a_taken_over_lead_not_proven_gone_reports_an_uncertain_cleanup
  team::runtime::tests::a_stopped_teammate_not_proven_gone_is_marked_uncertain
)
bad=0
note() {
  printf '%s\n' "$1" >> "$summary"
  printf '%s\n' "$1"
}
mutate() {
  local sites
  sites=$(grep -c "\"trap '' TERM;" "$file")
  sed -i '' "s/\"trap '' TERM;/\"sleep 1; trap '' TERM;/" "$file"
  note "| mutant | $sites site(s): sleep 1 before the trap, $(grep -c "\"sleep 1; trap '' TERM;" "$file") after sed |"
}
run() {
  local name="$1" sha="$2" mutant="$3" rounds="$4"
  git checkout -q --force --detach "$sha" || { note "| $name | checkout $sha FAILED |"; return 1; }
  if [ "$mutant" = yes ]; then mutate; fi
  if ! cargo test -p crazyagent --lib --locked --no-run > "$out/build-$name.txt" 2>&1; then
    note "| $name | lib test build FAILED |"
    grep -E -A12 '^error' "$out/build-$name.txt" | head -60
    return 1
  fi
  : > "$out/rows-$name.txt"
  for round in $(seq 1 "$rounds"); do
    echo "== round $round" >> "$out/rows-$name.txt"
    cargo test -p crazyagent --lib --locked --no-fail-fast -- --exact "${rows[@]}" >> "$out/rows-$name.txt" 2>&1
  done
  for row in "${rows[@]}"; do
    local ok failed
    ok=$(grep -c "^test $row \.\.\. ok" "$out/rows-$name.txt")
    failed=$(grep -c "^test $row \.\.\. FAILED" "$out/rows-$name.txt")
    printf '%s %s %s\n' "$row" "$ok" "$failed" >> "$out/counts-$name.txt"
    note "| $name | ${row##*::} | $ok ok, $failed FAILED of $rounds |"
  done
}
expect() {
  local name="$1" want="$2"
  while read -r _ ok failed; do
    if [ "$want" = FAILED ] && [ "$ok" != 0 ]; then bad=1; fi
    if [ "$want" = ok ] && [ "$failed" != 0 ]; then bad=1; fi
    if [ $((ok + failed)) -eq 0 ]; then bad=1; fi
  done < "$out/counts-$name.txt"
}
note "| side | row | result |"
note "| --- | --- | --- |"
run base-mutant "$base" yes 1 || exit 1
expect base-mutant FAILED
run tip "$tip" no 3 || exit 1
expect tip ok
run tip-mutant "$tip" yes 1 || exit 1
expect tip-mutant ok
git checkout -q --force --detach "$tip"
if cargo clippy -p crazyagent --tests --locked -- -D warnings > "$out/clippy.txt" 2>&1; then
  note "| clippy --tests -D warnings at tip | clean |"
else
  bad=1
  note "| clippy --tests -D warnings at tip | FAILED |"
  grep -E -A8 '^(error|warning)' "$out/clippy.txt" | head -60
fi
for name in base-mutant tip tip-mutant; do
  echo "== $name panics =="
  grep -E -B2 -A10 'panicked at' "$out/rows-$name.txt" | head -80
done
note "| verdict | $([ "$bad" -eq 0 ] && echo VERIFIED || echo NOT VERIFIED) |"
exit "$bad"
