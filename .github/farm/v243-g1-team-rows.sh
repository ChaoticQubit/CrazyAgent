set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
runtime=crates/crazyagent/src/team/runtime.rs
rows=(
  team::runtime::tests::the_master_framing_reads_only_a_bounded_regular_brief_and_never_blocks
  team::runtime::tests::a_failed_teammate_end_leaves_the_team_loop_to_its_tick
)
failed=0
printf '| run | expected | result |\n| --- | --- | --- |\n' >> "$summary"
panic_at() {
  awk -v h="---- $1 stdout ----" '$0 == h { f = 1; next } f && /^---- / { exit } f && /panicked at / { sub(/.*panicked at /, ""); sub(/:[0-9]+:.*/, ""); print; exit }' "$2"
}
cargo test -p crazyagent --lib --locked --no-fail-fast -- --exact "${rows[@]}" > "$RUNNER_TEMP/green-rows.txt" 2>&1
for row in "${rows[@]}"; do
  if grep -qF -- "test $row ... ok" "$RUNNER_TEMP/green-rows.txt"; then
    printf '| green %s at the fix tip | ok | ok |\n' "${row##*::}" >> "$summary"
  else
    printf '| green %s at the fix tip | ok | NOT OK, panic at %s |\n' "${row##*::}" "$(panic_at "$row" "$RUNNER_TEMP/green-rows.txt")" >> "$summary"
    failed=1
  fi
done
cat "$RUNNER_TEMP/green-rows.txt"
cargo test -p crazyagent --lib --locked --no-fail-fast -- team::runtime:: > "$RUNNER_TEMP/module.txt" 2>&1
module_rc=$?
for row in "${rows[@]}"; do
  seen=ok
  if ! grep -qF -- "test $row ... ok" "$RUNNER_TEMP/module.txt"; then
    seen="NOT OK"
    failed=1
  fi
  printf '| %s in the threaded team::runtime module | ok | %s |\n' "${row##*::}" "$seen" >> "$summary"
done
printf '| team::runtime module, threaded | pass | exit %s, %s, failed: %s |\n' "$module_rc" "$(grep -E '^test result' "$RUNNER_TEMP/module.txt" | tail -1)" "$(grep -E '^test .* FAILED$' "$RUNNER_TEMP/module.txt" | sed 's/^test //; s/ \.\.\. FAILED$//' | tr '\n' ' ')" >> "$summary"
grep -E '^test .* FAILED$|^test result' "$RUNNER_TEMP/module.txt"
cargo clippy -p crazyagent --all-targets --locked --message-format short -- -D warnings > "$RUNNER_TEMP/clippy.txt" 2>&1
clippy_rc=$?
printf '| macOS clippy -p crazyagent --all-targets -D warnings at the fix tip | exit 0 | exit %s, %s error lines |\n' "$clippy_rc" "$(grep -cE ': error: ' "$RUNNER_TEMP/clippy.txt")" >> "$summary"
[ "$clippy_rc" -eq 0 ] || failed=1
grep -E ': error: |^error' "$RUNNER_TEMP/clippy.txt"
mutate() {
  local before=$1 after=$2
  [ "$(grep -cxF -- "$before" "$runtime")" -eq 1 ] || { printf '| mutant line | found once | NOT FOUND: %s |\n' "$before" >> "$summary"; exit 1; }
  BEFORE="$before" AFTER="$after" perl -0pi -e 's/^\Q$ENV{BEFORE}\E$/$ENV{AFTER}/m' "$runtime"
  [ "$(grep -cxF -- "$after" "$runtime")" -eq 1 ] || { printf '| mutant line | written once | NOT WRITTEN: %s |\n' "$after" >> "$summary"; exit 1; }
}
mutate '                if settled {' '                if settled || id >= 0 {'
mutate '            let shown = bound_text(&text, BRIEF_BYTES_MAX);' '            let shown = bound_text(&text, BRIEF_BYTES_MAX + 1024);'
mutate '            assert!(shown.len() <= BRIEF_BYTES_MAX);' '            assert!(shown.len() <= BRIEF_BYTES_MAX + 1024);'
git diff --stat
cargo test -p crazyagent --lib --locked --no-fail-fast -- --exact "${rows[@]}" > "$RUNNER_TEMP/mutant-rows.txt" 2>&1
errors=$(grep -c 'error\[E' "$RUNNER_TEMP/mutant-rows.txt")
for row in "${rows[@]}"; do
  at=$(panic_at "$row" "$RUNNER_TEMP/mutant-rows.txt")
  if grep -qF -- "test $row ... FAILED" "$RUNNER_TEMP/mutant-rows.txt" && [ "$errors" -eq 0 ] && [[ "$at" == */team/runtime/tests.rs ]]; then
    printf '| red %s with the product mutant (end wakes always, brief cut 1024 bytes late) | fails at its assertion | fails at %s |\n' "${row##*::}" "$at" >> "$summary"
  else
    printf '| red %s with the product mutant | fails at its assertion | NOT: %s compile errors, panic at %s |\n' "${row##*::}" "$errors" "${at:-none}" >> "$summary"
    failed=1
  fi
done
cat "$RUNNER_TEMP/mutant-rows.txt"
git checkout HEAD -- "$runtime"
git diff --quiet || { printf '| tree | clean | DIRTY |\n' >> "$summary"; exit 1; }
exit "$failed"
