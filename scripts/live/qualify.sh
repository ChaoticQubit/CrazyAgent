#!/bin/sh
set -u
cd "$(dirname "$0")/../.." || exit 1
tests="engine::owner::transport::master_host_launch::live_claude_tests::a_real_claude_master_turn_is_signed_in_inside_the_master_sandbox
team::runtime::live_claude_tests::a_real_claude_team_agent_turn_is_signed_in_inside_the_agent_sandbox
research::runner::live_claude_tests::a_real_claude_research_run_is_signed_in_inside_the_research_sandbox
engine::owner::transport::master_host_launch::live_codex_tests::a_real_codex_master_turn_is_signed_in_inside_the_master_sandbox
team::runtime::live_codex_tests::a_real_codex_team_agent_turn_is_signed_in_inside_the_agent_sandbox
research::runner::live_codex_tests::a_real_codex_research_run_is_signed_in_inside_the_research_sandbox"
log=$(mktemp -d "${TMPDIR:-/tmp}/crazyagent-qualify.XXXXXX") || exit 1
trap 'rm -rf "$log"' EXIT
if ! cargo "$@" test --locked --offline -p crazyagent --lib --no-run >"$log/build" 2>&1; then
    tail -n 40 "$log/build"
    echo "FAIL build"
    exit 1
fi
failed=0
blocked=0
for test in $tests; do
    name=${test##*::}
    out="$log/$name"
    cargo "$@" test --locked --offline -p crazyagent --lib -- --ignored --exact "$test" --nocapture >"$out" 2>&1
    status=$?
    if grep -q "BLOCKED:" "$out"; then
        echo "BLOCKED $name: $(grep -o 'BLOCKED:.*' "$out" | head -n 1)"
        blocked=1
    elif grep -q "running 0 tests" "$out"; then
        echo "BLOCKED $name: not built on this platform"
        blocked=1
    elif [ "$status" -eq 0 ] && grep -q "test result: ok. 1 passed" "$out"; then
        echo "PASS $name: $(grep -E 'real (claude|codex)' "$out" | head -n 1)"
    else
        echo "FAIL $name"
        grep -E "real (claude|codex)|panicked at|left:|right:" -A 2 "$out" | head -n 20
        failed=1
    fi
done
if [ "$failed" -ne 0 ]; then
    exit 1
fi
if [ "$blocked" -ne 0 ]; then
    exit 2
fi
exit 0
