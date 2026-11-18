set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
red=472473a63
fix=274653c0b
host=rcli_host::master_process::tests::a_host_outlives_a_runtime_worker_that_handed_its_core_off
thread=a_nested_agent_outlives_the_thread_that_started_it
engine=a_kept_nested_agent_ends_with_the_engine
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
grep -E '^VERSION=' /etc/os-release
bwrap --version
git log --oneline -3
if [ "$(git rev-parse --short=9 HEAD~1)" != "$fix" ] || [ "$(git rev-parse --short=9 HEAD~2)" != "$red" ]; then
  row tips "HEAD~1 $fix, HEAD~2 $red" "$(git rev-parse --short=9 HEAD~1) $(git rev-parse --short=9 HEAD~2)"
  exit 1
fi
git diff --stat "$red" HEAD -- crates
judge() {
  local label=$1 file=$2 name=$3 expect=$4 message=$5 got at
  if grep -qF -- "test $name ... ok" "$file"; then
    got=passes
  elif grep -qF -- "test $name ... FAILED" "$file"; then
    at=$(grep -m1 -oE 'panicked at [^:]+:[0-9]+' "$file" | sed 's/panicked at //')
    if [ -n "$message" ] && grep -qF -- "$message" "$file"; then
      got="fails: $message"
    else
      got="fails elsewhere at ${at:-no panic line}"
    fi
  else
    got="not run"
  fi
  echo "== $name at $label: $got"
  tail -25 "$file"
  if [ "$got" = "$expect" ]; then
    row "$name at $label" "$expect" "$got ${at:+($at)}"
  else
    row "$name at $label" "$expect" "NOT: $got"
    failed=1
  fi
}
rows() {
  local label=$1 host_expect=$2 thread_expect=$3
  cargo test -p crazyagent --lib --locked -- "$host" --exact > "$RUNNER_TEMP/$label-host.txt" 2>&1
  judge "$label" "$RUNNER_TEMP/$label-host.txt" "$host" "$host_expect" "the host ended with the runtime worker that started it"
  cargo test -p crazyagent --test linux_sandbox --locked -- "$thread" --exact > "$RUNNER_TEMP/$label-thread.txt" 2>&1
  judge "$label" "$RUNNER_TEMP/$label-thread.txt" "$thread" "$thread_expect" "the agent ended with the thread that started it"
  cargo test -p crazyagent --test linux_sandbox --locked -- "$engine" --exact > "$RUNNER_TEMP/$label-engine.txt" 2>&1
  judge "$label" "$RUNNER_TEMP/$label-engine.txt" "$engine" passes "the agent outlived the engine that kept it"
}
suites() {
  local label=$1
  cargo test -p crazyagent --test linux_sandbox --locked --no-fail-fast > "$RUNNER_TEMP/$label-file.txt" 2>&1
  grep -E '^test .* FAILED$|^test result' "$RUNNER_TEMP/$label-file.txt"
  grep -E '^test .* FAILED$' "$RUNNER_TEMP/$label-file.txt" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sort > "$RUNNER_TEMP/$label-file-failed.txt"
  row "linux_sandbox at $label" "record" "$(grep -E '^test result' "$RUNNER_TEMP/$label-file.txt" | tail -1); failed: $(tr '\n' ' ' < "$RUNNER_TEMP/$label-file-failed.txt")"
  cargo test -p crazyagent --lib --locked --no-fail-fast -- rcli_host:: spawn_gate:: master_rcli > "$RUNNER_TEMP/$label-lib.txt" 2>&1
  grep -E '^test .* FAILED$|^test result' "$RUNNER_TEMP/$label-lib.txt"
  grep -E '^test .* FAILED$' "$RUNNER_TEMP/$label-lib.txt" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sort > "$RUNNER_TEMP/$label-lib-failed.txt"
  row "lib rcli_host, spawn_gate, master_rcli at $label" "record" "$(grep -E '^test result' "$RUNNER_TEMP/$label-lib.txt" | tail -1); failed: $(tr '\n' ' ' < "$RUNNER_TEMP/$label-lib-failed.txt")"
}
echo "== red tip $red"
git checkout "$red" -- crates
git diff --stat HEAD -- crates
rows "$red" "fails: the host ended with the runtime worker that started it" "fails: the agent ended with the thread that started it"
suites "$red"
echo "== fix tip $fix"
git checkout HEAD -- crates
[ -z "$(git status --porcelain -- crates)" ] || { row "restore $fix" "clean crates" "$(git status --porcelain -- crates | head -3 | tr '\n' ' ')"; exit 1; }
rows "$fix" passes passes
suites "$fix"
for part in file lib; do
  added=$(comm -13 "$RUNNER_TEMP/$red-$part-failed.txt" "$RUNNER_TEMP/$fix-$part-failed.txt" | tr '\n' ' ')
  cleared=$(comm -23 "$RUNNER_TEMP/$red-$part-failed.txt" "$RUNNER_TEMP/$fix-$part-failed.txt" | tr '\n' ' ')
  if [ -n "$added" ]; then
    row "$part reds new at $fix" "none" "NOT: $added"
    failed=1
  else
    row "$part reds new at $fix" "none" "none; cleared: ${cleared:-none}"
  fi
done
exit "$failed"
