set -uo pipefail
os="${1:?linux or macos}"
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/verify"
mkdir -p "$out"
red=3d49b9a1a7a063d1d28c4dfa814fec2cb6e641d7
tip=b934e8fcf87aa364ec76ba1724a0b1c4ab1cccd6
modules=(tools:: harnesses:: setup:: sandbox::)
flips=(
  tools::tests::an_npm_prefix_under_a_sticky_shared_folder_starts_with_its_own_node
  harnesses::interactive::tests::a_shim_under_a_sticky_shared_folder_starts_as_given_by_the_resolver_rule
)
linux_rows=(
  tools::tests::an_npm_script_starts_with_the_node_of_its_own_prefix
  harnesses::interactive::tests::a_shim_harness_starts_its_real_install_or_else_itself_unless_agents_can_write_it
  tools::tests::a_group_writable_folder_is_passed_over_unless_its_group_is_admin
  setup::pinned::tests::the_permission_fix_acts_only_on_the_target_the_installer_resolved_and_confirmed
)
new_rows=(tools::tests::a_passed_over_own_node_falls_back_and_is_logged_once_per_file)
if [ "$os" = linux ]; then
  flips+=("${linux_rows[@]}")
fi
bad=0
note() {
  printf '%s\n' "$1" >> "$summary"
  printf '%s\n' "$1"
}
side() {
  local name="$1" sha="$2"
  git checkout -q --detach "$sha" || { note "| $name | checkout $sha FAILED |"; return 1; }
  if ! cargo test -p crazyagent --lib --locked --no-run > "$out/build-$name.txt" 2>&1; then
    note "| $name | lib test build FAILED at $sha |"
    grep -E -A12 '^error' "$out/build-$name.txt" | head -60
    return 1
  fi
  cargo test -p crazyagent --lib --locked --no-fail-fast -- "${modules[@]}" > "$out/rows-$name.txt" 2>&1
  grep -E '^test [^ ]+ \.\.\. (ok|FAILED|ignored)' "$out/rows-$name.txt" \
    | sed -E 's/^test ([^ ]+) \.\.\. ([A-Za-z]+).*/\1 \2/' | sort -u > "$out/results-$name.txt"
  note "| $name $sha | $(grep -c ' ok$' "$out/results-$name.txt") ok, $(grep -c ' FAILED$' "$out/results-$name.txt") FAILED |"
}
result() {
  awk -v row="$2" '$1 == row {print $2; exit}' "$out/results-$1.txt"
}
note "| side | rows |"
note "| --- | --- |"
side red "$red" || exit 1
side tip "$tip" || exit 1
note ""
note "| row | red | tip | expected |"
note "| --- | --- | --- | --- |"
for row in "${flips[@]}"; do
  r=$(result red "$row"); t=$(result tip "$row")
  if [ "$r" != FAILED ] || [ "$t" != ok ]; then bad=1; fi
  note "| ${row##*::} | ${r:-absent} | ${t:-absent} | FAILED then ok |"
done
for row in "${linux_rows[@]}" "${new_rows[@]}"; do
  case " ${flips[*]} " in *" $row "*) continue ;; esac
  r=$(result red "$row"); t=$(result tip "$row")
  if [ "$t" != ok ]; then bad=1; fi
  note "| ${row##*::} | ${r:-absent} | ${t:-absent} | ok at tip |"
done
awk '$2 == "FAILED" {print $1}' "$out/results-red.txt" | sort > "$out/failed-red.txt"
awk '$2 == "FAILED" {print $1}' "$out/results-tip.txt" | sort > "$out/failed-tip.txt"
comm -13 "$out/failed-red.txt" "$out/failed-tip.txt" > "$out/regressed.txt"
note ""
note "| check | value |"
note "| --- | --- |"
note "| FAILED at tip, not at red | $(wc -l < "$out/regressed.txt") |"
note "| FAILED at both (not this branch) | $(comm -12 "$out/failed-red.txt" "$out/failed-tip.txt" | wc -l) |"
if [ -s "$out/regressed.txt" ]; then
  bad=1
  while IFS= read -r row; do note "| regressed | $row |"; done < "$out/regressed.txt"
fi
comm -12 "$out/failed-red.txt" "$out/failed-tip.txt" | while IFS= read -r row; do note "| red at both | $row |"; done
if [ "$os" = macos ]; then
  for target in sandbox_toolchains clean_mac; do
    if ! cargo test -p crazyagent --test "$target" --locked --no-run > "$out/build-$target.txt" 2>&1; then
      note "| $target | build FAILED |"
      grep -E -A12 '^error' "$out/build-$target.txt" | head -60
      bad=1
      continue
    fi
    cargo test -p crazyagent --test "$target" --locked --no-fail-fast > "$out/rows-$target.txt" 2>&1
    note "| $target at tip | $(grep -E -c '^test .* \.\.\. ok' "$out/rows-$target.txt") ok, $(grep -E -c '^test .* \.\.\. FAILED' "$out/rows-$target.txt") FAILED |"
    grep -E '^test .* \.\.\. FAILED' "$out/rows-$target.txt" | while IFS= read -r line; do note "| $target FAILED | $line |"; done
  done
fi
for scope in --lib --tests; do
  if cargo clippy -p crazyagent "$scope" --locked -- -D warnings > "$out/clippy$scope.txt" 2>&1; then
    note "| clippy $scope -D warnings | clean |"
  else
    bad=1
    note "| clippy $scope -D warnings | FAILED |"
    grep -E -A8 '^(error|warning)' "$out/clippy$scope.txt" | head -60
  fi
done
for name in red tip; do
  grep -E -B2 -A14 'panicked at' "$out/rows-$name.txt" | head -150 > "$out/panics-$name.txt"
done
echo "== tip panics =="
cat "$out/panics-tip.txt"
note "| verdict | $([ "$bad" -eq 0 ] && echo VERIFIED || echo NOT VERIFIED) |"
exit "$bad"
