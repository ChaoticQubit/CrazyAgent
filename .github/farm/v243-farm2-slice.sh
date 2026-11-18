set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/slice"
mkdir -p "$out"
base=556987b3612542ce23bd10fd9bd366216867a838
tip=1f743065855bdf825df7772548eb36db4c55747b
lib_rows=(launch_context::tests::a_skills_folder_without_write_is_opened_up_through_its_descriptor_never_its_name)
tui_rows=(
  a_removed_project_comes_back_only_from_its_own_folder_in_the_tui
  after_delete_everything_the_same_folder_is_a_new_project_in_the_tui
)
flips=(
  "${lib_rows[@]}"
  the_codex_system_skills_flag_deny_compiles_clean_and_holds_for_every_spelling
  "${tui_rows[@]}"
)
orphan_base=a_full_access_headless_stop_misses_an_orphan_made_between_two_tables
orphan_tip=a_full_access_headless_stop_names_or_misses_an_orphan_made_between_two_tables
bad=0
note() {
  printf '%s\n' "$1" >> "$summary"
  printf '%s\n' "$1"
}
built() {
  local name="$1" label="$2"
  shift 2
  if cargo test -p crazyagent --locked "$@" --no-run > "$out/build-$name-$label.txt" 2>&1; then
    return 0
  fi
  note "| $name | $label build FAILED |"
  grep -E -A12 '^error' "$out/build-$name-$label.txt" | head -60
  return 1
}
side() {
  local name="$1" sha="$2"
  git checkout -q --detach "$sha" || { note "| $name | checkout $sha FAILED |"; return 1; }
  built "$name" lib --lib || return 1
  for target in sandbox_regex_compile e2e_tui harness_launch; do
    built "$name" "$target" --test "$target" || return 1
  done
  {
    cargo test -p crazyagent --lib --locked --no-fail-fast -- --exact "${lib_rows[@]}"
    cargo test -p crazyagent --test sandbox_regex_compile --locked --no-fail-fast
    cargo test -p crazyagent --test e2e_tui --locked --no-fail-fast -- --exact "${tui_rows[@]}"
    cargo test -p crazyagent --test harness_launch --locked --no-fail-fast -- an_orphan_made_between_two_tables
  } > "$out/rows-$name.txt" 2>&1
  grep -E '^test [^ ]+ \.\.\. (ok|FAILED|ignored)' "$out/rows-$name.txt" \
    | sed -E 's/^test ([^ ]+) \.\.\. ([A-Za-z]+).*/\1 \2/' | sort -u > "$out/results-$name.txt"
  note "| $name $sha | $(grep -c ' ok$' "$out/results-$name.txt") ok, $(grep -c ' FAILED$' "$out/results-$name.txt") FAILED |"
}
result() {
  awk -v row="$2" '$1 == row {print $2; exit}' "$out/results-$1.txt"
}
note "| side | rows |"
note "| --- | --- |"
side base "$base" || exit 1
side tip "$tip" || exit 1
note ""
note "| row | base | tip | expected |"
note "| --- | --- | --- | --- |"
for row in "${flips[@]}"; do
  r=$(result base "$row"); t=$(result tip "$row")
  if [ "$r" != FAILED ] || [ "$t" != ok ]; then bad=1; fi
  note "| ${row##*::} | ${r:-absent} | ${t:-absent} | FAILED then ok |"
done
r=$(result base "$orphan_base"); t=$(result tip "$orphan_tip")
if [ "$t" != ok ]; then bad=1; fi
note "| orphan row (renamed at tip) | ${r:-absent} | ${t:-absent} | either at base, ok at tip |"
awk '$2 == "FAILED" {print $1}' "$out/results-base.txt" | sort > "$out/failed-base.txt"
awk '$2 == "FAILED" {print $1}' "$out/results-tip.txt" | sort > "$out/failed-tip.txt"
comm -13 "$out/failed-base.txt" "$out/failed-tip.txt" | grep -v -x "$orphan_tip" > "$out/regressed.txt"
note ""
note "| check | value |"
note "| --- | --- |"
note "| FAILED at tip, not at base | $(wc -l < "$out/regressed.txt") |"
if [ -s "$out/regressed.txt" ]; then
  bad=1
  while IFS= read -r row; do note "| regressed | $row |"; done < "$out/regressed.txt"
fi
comm -12 "$out/failed-base.txt" "$out/failed-tip.txt" | while IFS= read -r row; do note "| red at both | $row |"; done
if cargo clippy -p crazyagent --tests --locked -- -D warnings > "$out/clippy.txt" 2>&1; then
  note "| clippy --tests -D warnings at tip | clean |"
else
  bad=1
  note "| clippy --tests -D warnings at tip | FAILED |"
  grep -E -A8 '^(error|warning)' "$out/clippy.txt" | head -60
fi
for name in base tip; do
  echo "== $name panics =="
  grep -E -B2 -A14 'panicked at' "$out/rows-$name.txt" | head -200
done
note "| verdict | $([ "$bad" -eq 0 ] && echo VERIFIED || echo NOT VERIFIED) |"
exit "$bad"
