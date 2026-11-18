set +e -uo pipefail
label=${1:?usage: research-loopback.sh <label>}
red=c0d3624ca
file=crates/crazyagent/src/research/runner.rs
out="$RUNNER_TEMP/research-loopback-$label.txt"
table="$RUNNER_TEMP/research-loopback-$label.md"
git log --oneline -1 | tee "$out"
printf '| %s step | exit |\n| --- | --- |\n' "$label" > "$table"
fail=0
step() {
  local name=$1
  shift
  "$@" 2>&1 | tee -a "$out"
  local rc=${PIPESTATUS[0]}
  echo "== $name rc=$rc" | tee -a "$out"
  printf '| %s | %s |\n' "$name" "$rc" >> "$table"
  [ "$rc" = 0 ] || fail=1
}
cargo=(cargo +"$RUST_TOOLCHAIN")
rows=(
  research::runner::tests::a_research_job_on_windows_with_a_model_on_this_computer_is_refused_under_every_choice
  research::runner::tests::a_windows_research_job_with_tools_under_no_network_starts_no_model_proxy
  research::runner::tests::a_research_job_with_tools_under_no_network_reaches_its_model_only_through_a_model_proxy
  research::runner::tests::linux_and_windows_refuse_team_agents_and_research_with_a_plain_line
)
case "$RUNNER_OS" in
  Linux)
    step "green test --lib --no-run" "${cargo[@]}" test -p crazyagent --lib --locked --no-run
    step "green new and neighbour rows" "${cargo[@]}" test -p crazyagent --lib --locked --no-fail-fast -- --exact "${rows[@]}"
    step "green research:: and sandbox::policy" "${cargo[@]}" test -p crazyagent --lib --locked --no-fail-fast -- research:: sandbox::policy
    step "clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    git checkout "$red" -- "$file"
    git diff --stat HEAD -- "$file" | tee -a "$out"
    "${cargo[@]}" test -p crazyagent --lib --locked --no-run 2>&1 | tail -3 | tee -a "$out"
    "${cargo[@]}" test -p crazyagent --lib --locked --no-fail-fast -- --exact "${rows[0]}" "${rows[1]}" 2>&1 | tee "$RUNNER_TEMP/red.txt" | tee -a "$out"
    redrc=${PIPESTATUS[0]}
    failed=$(grep -cE '^test research::runner::tests::(a_research_job_on_windows_with_a_model_on_this_computer_is_refused_under_every_choice|a_windows_research_job_with_tools_under_no_network_starts_no_model_proxy) \.\.\. FAILED$' "$RUNNER_TEMP/red.txt")
    echo "== red rc=$redrc failed_rows=$failed (want nonzero and 2)" | tee -a "$out"
    printf '| red at %s: both new rows fail | rc=%s, failed=%s/2 |\n' "$red" "$redrc" "$failed" >> "$table"
    { [ "$redrc" != 0 ] && [ "$failed" = 2 ]; } || fail=1
    git checkout HEAD -- "$file"
    ;;
  Windows)
    step "clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  macOS)
    step "test --lib --no-run" "${cargo[@]}" test -p crazyagent --lib --locked --no-run
    step "macOS profile row and new rows" "${cargo[@]}" test -p crazyagent --lib --locked --no-fail-fast -- --exact research::runner::tests::a_research_job_network_follows_the_project_choice_only_with_tools "${rows[@]}"
    step "clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  *)
    echo "::error::no recipe for RUNNER_OS=$RUNNER_OS"
    exit 1
    ;;
esac
failed=$(awk '/^test .* \.\.\. FAILED$/ {print $2}' "$out" | sort -u)
{
  cat "$table"
  echo
  echo "Rows reported FAILED (red step included): $(printf '%s' "$failed" | grep -c .)"
  [ -z "$failed" ] || printf -- '- %s\n' $failed
} >> "$GITHUB_STEP_SUMMARY"
exit "$fail"
