set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
base=f420569df
row() { printf '| %s | %s |\n' "$1" "$2" >> "$summary"; }
printf '| probe | exit and stderr |\n| --- | --- |\n' >> "$summary"
uname -r
echo "lsm: $(cat /sys/kernel/security/lsm 2>&1)"
sysctl net.ipv4.ip_local_port_range
git log --oneline -2
code_tip=$(git log -1 --format=%H HEAD -- . ':!.github')
[ "$(git rev-parse --short=9 "$code_tip")" = "$base" ] || { row tip "NOT: $(git rev-parse --short=9 "$code_tip"), not $base"; exit 1; }
cargo build -p crazyagent --bin crazyagent-landlock --locked 2>&1 | tail -2
helper="$PWD/target/debug/crazyagent-landlock"
probe() {
  local said code
  said=$(env -i "$helper" "$@" < /dev/null 2>&1 >/dev/null)
  code=$?
  echo "== $helper $* -> exit $code: $said"
  row "--probe ${*:2}" "exit $code $(printf '%s' "$said" | tr '\n' ' ' | cut -c1-300)"
}
probe --probe
probe --probe --limit-network
probe --probe --limit-network --open-sockets
probe --probe --keep-free 1455
probe --probe --keep-free 11434
cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -2
bin=$(find target/debug/deps -maxdepth 1 -type f -executable -name 'linux_sandbox-*' -printf '%T@ %p\n' | sort -rn | head -1 | cut -d' ' -f2)
for name in an_installed_sandbox_on_linux_launches_through_the_linux_boundary the_strict_setting_refuses_a_launch_without_the_call_supervisor_and_off_starts_it; do
  "$bin" --exact "$name" > "$RUNNER_TEMP/$name.txt" 2>&1
  echo "== $name"
  grep -E 'panicked at|Unavailable|^test ' "$RUNNER_TEMP/$name.txt" | head -6
  row "$name" "$(grep -m1 -E '^test ' "$RUNNER_TEMP/$name.txt"): $(grep -m1 -oE 'Unavailable\([^)]*\)' "$RUNNER_TEMP/$name.txt" | cut -c1-200)"
done
exit 0
