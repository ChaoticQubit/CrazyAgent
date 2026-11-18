set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/probe"
mkdir -p "$out"
base=92247072d8032bde9508034029adf93973cf976a
tip=50bc50eebf37cdc4f8511047070dfcb388dfa675
flips=(
  the_call_supervisor_probe_holds_on_this_machine_and_fails_with_nothing_supervising
  a_helper_told_to_die_with_its_parent_is_killed_when_that_parent_exits
)
bad=0
note() {
  printf '%s\n' "$1" >> "$summary"
  printf '%s\n' "$1"
}
side() {
  local name="$1" sha="$2"
  git checkout -q --detach "$sha" || { note "| $name | checkout $sha FAILED |"; return 1; }
  if ! cargo test -p crazyagent --test linux_sandbox --locked --no-run > "$out/build-$name.txt" 2>&1; then
    note "| $name | linux_sandbox build FAILED at $sha |"
    grep -E -A12 '^error' "$out/build-$name.txt" | head -60
    return 1
  fi
  cargo test -p crazyagent --test linux_sandbox --locked --no-fail-fast > "$out/rows-$name.txt" 2>&1
  grep -E '^test [^ ]+ \.\.\. (ok|FAILED|ignored)' "$out/rows-$name.txt" \
    | sed -E 's/^test ([^ ]+) \.\.\. ([A-Za-z]+).*/\1 \2/' | sort -u > "$out/results-$name.txt"
  note "| $name $sha | $(grep -c ' ok$' "$out/results-$name.txt") ok, $(grep -c ' FAILED$' "$out/results-$name.txt") FAILED |"
}
result() {
  awk -v row="$2" '$1 == row {print $2; exit}' "$out/results-$1.txt"
}
{
  printf '| fact | value |\n| --- | --- |\n'
  printf '| kernel | %s |\n' "$(uname -r)"
  printf '| ptrace_scope | %s |\n' "$(cat /proc/sys/kernel/yama/ptrace_scope 2>/dev/null || echo none)"
  printf '| LD_ vars in job env | %s |\n' "$(env | grep -c '^LD_')"
} >> "$summary"
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
  note "| $row | ${r:-absent} | ${t:-absent} | FAILED then ok |"
done
awk '$2 == "FAILED" {print $1}' "$out/results-base.txt" | sort > "$out/failed-base.txt"
awk '$2 == "FAILED" {print $1}' "$out/results-tip.txt" | sort > "$out/failed-tip.txt"
comm -13 "$out/failed-base.txt" "$out/failed-tip.txt" > "$out/regressed.txt"
note ""
note "| check | value |"
note "| --- | --- |"
note "| FAILED at tip, not at base | $(wc -l < "$out/regressed.txt") |"
if [ -s "$out/regressed.txt" ]; then
  bad=1
  while IFS= read -r row; do note "| regressed | $row |"; done < "$out/regressed.txt"
fi
comm -12 "$out/failed-base.txt" "$out/failed-tip.txt" | while IFS= read -r row; do note "| red at both | $row |"; done
if cargo clippy -p crazyagent --test linux_sandbox --locked -- -D warnings > "$out/clippy.txt" 2>&1; then
  note "| clippy --test linux_sandbox -D warnings | clean |"
else
  bad=1
  note "| clippy --test linux_sandbox -D warnings | FAILED |"
  grep -E -A8 '^(error|warning)' "$out/clippy.txt" | head -60
fi
for name in base tip; do
  echo "== $name panics =="
  grep -E -B2 -A14 'panicked at' "$out/rows-$name.txt" | head -200
done
note "| verdict | $([ "$bad" -eq 0 ] && echo VERIFIED || echo NOT VERIFIED) |"
exit "$bad"
