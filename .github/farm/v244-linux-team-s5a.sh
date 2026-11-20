set -uo pipefail
kind=${1:?host kind: linux or windows}
out="$RUNNER_TEMP/v244-linux-team-s5a-$kind.txt"
tip=4286d327b
struct_commit=3c2bf1aa3
failed=0
: > "$out"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" | tee -a "$out"
}
code_tip=$(git log -1 --format=%h --abbrev=9 HEAD -- . ':!.github')
if [ "$code_tip" != "$tip" ]; then
  row "code tip" "$tip" "NOT: $code_tip"
  exit 1
fi
farm_head=$(git rev-parse HEAD)
row "code tip" "$tip" "$code_tip"
row "host" "$kind" "$(uname -srm)"
block() {
  local name=$1 file=$2
  awk -v head="---- $name stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$file" | head -40
}
results() {
  local label=$1 file=$2
  grep -E '^test result:' "$file" | tee -a "$out" || true
  local counts
  counts=$(grep -E '^test result:' "$file" | awk '{for (i = 1; i <= NF; i++) { if ($(i+1) ~ /^(passed|failed|ignored|filtered)/) { k = $(i+1); sub(/[;,]/, "", k); n[k] += $i } } b++} END {printf "binaries %d passed %d failed %d ignored %d filtered %d", b, n["passed"], n["failed"], n["ignored"], n["filtered"]}')
  row "$label" record "$counts"
}
reds() {
  grep -E '^test .* FAILED$' "$1" | awk '{print $2}' | sort | tr '\n' ' ' || true
}
cargo check -p crazyagent --all-targets --locked --message-format short > "$RUNNER_TEMP/check-$kind.txt" 2>&1
rc=$?
grep -E '^(error|warning)' "$RUNNER_TEMP/check-$kind.txt" | head -60 | tee -a "$out"
issues=$(grep -cE '^(error|warning)' "$RUNNER_TEMP/check-$kind.txt" || true)
if [ "$rc" -eq 0 ] && [ "${issues:-0}" -eq 0 ]; then
  row "cargo check -p crazyagent --all-targets at $tip" "rc 0, 0 warnings" "rc 0, 0 warnings"
else
  row "cargo check -p crazyagent --all-targets at $tip" "rc 0, 0 warnings" "NOT: rc $rc, $issues errors or warnings"
  tail -30 "$RUNNER_TEMP/check-$kind.txt" | tee -a "$out"
  failed=1
fi
if [ "$kind" != linux ]; then
  echo "S5a $kind failed=$failed" | tee -a "$out"
  exit "$failed"
fi
new_rows="a_linux_launch_starts_at_its_installed_boundary_with_the_agent_environment a_full_access_launch_on_linux_is_refused_and_builds_nothing a_linux_launch_with_another_home_than_its_boundary_is_refused"
expected_red="a_linux_launch_starts_at_its_installed_boundary_with_the_agent_environment a_linux_launch_with_another_home_than_its_boundary_is_refused "
git checkout -q -f --detach "$struct_commit"
git checkout -q "$tip" -- crates/crazyagent/tests/linux_sandbox.rs
cargo test -p crazyagent --test linux_sandbox --locked -- $new_rows --exact > "$RUNNER_TEMP/red-$kind.txt" 2>&1
results "rows at $tip on $struct_commit (gate kept)" "$RUNNER_TEMP/red-$kind.txt"
got=$(reds "$RUNNER_TEMP/red-$kind.txt")
for name in $got; do
  echo "-- red $name" | tee -a "$out"
  block "$name" "$RUNNER_TEMP/red-$kind.txt" | grep -A4 'panicked at' | head -6 | tee -a "$out"
done
if [ "$got" = "$expected_red" ]; then
  row "red with the gate kept" "$expected_red" "$got"
else
  row "red with the gate kept" "$expected_red" "NOT: ${got:-none}"
  failed=1
fi
git checkout -q -f --detach "$farm_head"
cargo test -p crazyagent --test linux_sandbox --locked > "$RUNNER_TEMP/box-$kind.txt" 2>&1
results "linux_sandbox at $tip" "$RUNNER_TEMP/box-$kind.txt"
got=$(reds "$RUNNER_TEMP/box-$kind.txt")
for name in $got; do
  echo "-- red $name" | tee -a "$out"
  block "$name" "$RUNNER_TEMP/box-$kind.txt" | grep -A4 'panicked at' | head -6 | tee -a "$out"
done
for name in $new_rows; do
  if grep -qE "^test $name \.\.\. ok$" "$RUNNER_TEMP/box-$kind.txt"; then row "$name" ok ok; else row "$name" ok NOT; failed=1; fi
done
if [ -n "$got" ] || ! grep -qE '^test result: ok' "$RUNNER_TEMP/box-$kind.txt"; then
  row "linux_sandbox reds" none "NOT: ${got:-no ok result}"
  tail -20 "$RUNNER_TEMP/box-$kind.txt" | tee -a "$out"
  failed=1
else
  row "linux_sandbox reds" none none
fi
exe=$(cargo test -p crazyagent --lib --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-lib-$kind.txt" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))$/\1/p' | head -1)
if [ -z "$exe" ] || [ ! -x "$exe" ]; then
  row "lib test binary" built "NOT: none"
  tail -30 "$RUNNER_TEMP/build-lib-$kind.txt" | tee -a "$out"
  exit 1
fi
exe=$(realpath "$exe")
for filter in harnesses::launch:: team::runtime::tests::each_start_runs_at_the_saved_level_and_a_lowered_clone_is_refused team::runtime::tests::a_native_lead_plan_is_refused_at_full_access_and_starts_inside_its_profile_otherwise; do
  name=$(echo "$filter" | tr ':' '_' | cut -c1-50)
  (cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" "$exe" "$filter" > "$RUNNER_TEMP/lib-$name-$kind.txt" 2>&1)
  results "lib $filter" "$RUNNER_TEMP/lib-$name-$kind.txt"
  got=$(reds "$RUNNER_TEMP/lib-$name-$kind.txt")
  passed=$(grep -E '^test result:' "$RUNNER_TEMP/lib-$name-$kind.txt" | awk '{print $4}' | head -1)
  if [ -n "$got" ] || { [ "$filter" = harnesses::launch:: ] && [ "${passed:-0}" = 0 ]; }; then
    row "lib $filter reds" "none, at least 1 row" "NOT: ${got:-0 rows}"
    for red in $got; do block "$red" "$RUNNER_TEMP/lib-$name-$kind.txt" | grep -A4 'panicked at' | head -6 | tee -a "$out"; done
    failed=1
  else
    row "lib $filter reds" "none" "none, ${passed:-0} rows"
  fi
done
echo "S5a $kind failed=$failed" | tee -a "$out"
exit "$failed"
