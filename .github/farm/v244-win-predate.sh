set -uo pipefail
label=${1:?usage: v244-win-predate.sh <label>}
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/predate"
mkdir -p "$out"
base=5e25980d6f2f91bd9fe53e54e21cbc8d76c2380b
tip=6bd9123c6fd207bfb2a98b9a5f477ca20e0e578e
mine='probe_tests\.rs|accounts_tests\.rs'
rows=(
  harnesses::probe_tests::a_cached_catalog_is_served_while_its_re_probe_runs_and_a_check_probes_now
  harnesses::probe_tests::a_cached_catalog_whose_harness_file_is_gone_is_probed_before_it_answers
  research::accounts::accounts_tests::research_runs_on_endpoint_accounts_with_or_without_a_key_by_wire_format
)
modules=(harnesses::probe_tests research::accounts::accounts_tests)
bad=0
note() {
  printf '%s\n' "$1" >> "$summary"
  printf '%s\n' "$1"
}
side() {
  local name="$1" sha="$2" rounds="$3"
  git checkout -q --force --detach "$sha" || { note "| $label $name | checkout $sha FAILED |"; return 1; }
  if ! cargo test -p crazyagent --lib --locked --no-run > "$out/build-$name.txt" 2>&1; then
    note "| $label $name | lib test build FAILED |"
    tr -d '\r' < "$out/build-$name.txt" | grep -E -A12 '^error' | head -60
    return 1
  fi
  : > "$out/rows-$name.txt"
  for round in $(seq 1 "$rounds"); do
    echo "== round $round" >> "$out/rows-$name.txt"
    cargo test -p crazyagent --lib --locked --no-fail-fast -- "${modules[@]}" 2>&1 | tr -d '\r' >> "$out/rows-$name.txt"
  done
  grep -E '^test [^ ]+ \.\.\. FAILED' "$out/rows-$name.txt" | awk '{print $2}' | sort -u > "$out/failed-$name.txt"
  note "| $label $name ${sha:0:9} | $rounds round(s), $(grep -cE '^test [^ ]+ \.\.\. ok' "$out/rows-$name.txt") ok, $(grep -cE '^test [^ ]+ \.\.\. FAILED' "$out/rows-$name.txt") FAILED |"
}
count() {
  grep -c "^test $2 \.\.\. $3" "$out/rows-$1.txt"
}
note "| side | rows in ${modules[*]} |"
note "| --- | --- |"
if [ "$RUNNER_OS" = Windows ]; then
  side base "$base" 1 || exit 1
fi
side tip "$tip" 3 || exit 1
note ""
note "| row | base FAILED | tip ok | tip FAILED |"
note "| --- | --- | --- | --- |"
for row in "${rows[@]}"; do
  b=absent
  if [ "$RUNNER_OS" = Windows ]; then
    b=$(count base "$row" FAILED)
    [ "$b" = 1 ] || bad=1
  fi
  ok=$(count tip "$row" ok)
  failed=$(count tip "$row" FAILED)
  { [ "$ok" = 3 ] && [ "$failed" = 0 ]; } || bad=1
  note "| ${row##*::} | $b | $ok | $failed |"
done
while IFS= read -r row; do
  if [ -f "$out/failed-base.txt" ] && grep -qxF "$row" "$out/failed-base.txt"; then
    note "| FAILED at base and tip | $row |"
  else
    bad=1
    note "| FAILED at tip only | $row |"
  fi
done < "$out/failed-tip.txt"
if cargo clippy -p crazyagent --lib --profile test --locked -- -D warnings > "$out/clippy.txt" 2>&1; then
  note "| clippy lib in test mode, -D warnings, at tip | clean |"
else
  named=$(tr -d '\r' < "$out/clippy.txt" | grep -cE "$mine")
  [ "$named" = 0 ] || bad=1
  note "| clippy lib in test mode, -D warnings, at tip | FAILED, $named line(s) name the two changed files |"
  tr -d '\r' < "$out/clippy.txt" | grep -E -A8 '^(error|warning)' | head -80
fi
for name in base tip; do
  [ -f "$out/rows-$name.txt" ] || continue
  echo "== $name panics =="
  grep -E -A6 'panicked at' "$out/rows-$name.txt" | head -80
done
note "| verdict $label | $([ "$bad" -eq 0 ] && echo VERIFIED || echo NOT VERIFIED) |"
exit "$bad"
