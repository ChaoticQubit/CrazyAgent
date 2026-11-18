set +e -uo pipefail
label=${1:?usage: research-loopback.sh <label>}
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
  research::runner::tests::a_research_job_network_follows_the_project_choice_only_with_tools
)
case "$RUNNER_OS" in
  Linux)
    step "green test --lib --no-run" "${cargo[@]}" test -p crazyagent --lib --locked --no-run
    step "green new and neighbour rows" "${cargo[@]}" test -p crazyagent --lib --locked --no-fail-fast -- --exact "${rows[@]}"
    step "green research:: and sandbox::policy" "${cargo[@]}" test -p crazyagent --lib --locked --no-fail-fast -- research:: sandbox::policy
    step "clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  Windows)
    step "clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  macOS)
    step "test --lib --no-run" "${cargo[@]}" test -p crazyagent --lib --locked --no-run
    step "new rows and the profile row" "${cargo[@]}" test -p crazyagent --lib --locked --no-fail-fast -- --exact "${rows[@]}"
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
  echo "Rows reported FAILED: $(printf '%s' "$failed" | grep -c .)"
  [ -z "$failed" ] || printf -- '- %s\n' $failed
} >> "$GITHUB_STEP_SUMMARY"
exit "$fail"
