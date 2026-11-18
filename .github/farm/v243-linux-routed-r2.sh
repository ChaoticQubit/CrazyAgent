set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
fix=27d1c6323
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
grep -E '^VERSION=' /etc/os-release
/usr/bin/slirp4netns --version 2>&1 | head -1
bwrap --version
git log --oneline -2
if [ "$(git rev-parse --short=9 HEAD~1)" != "$fix" ]; then
  row tip "HEAD~1 $fix" "$(git rev-parse --short=9 HEAD~1)"
  exit 1
fi
block() {
  local name=$1 file=$2
  awk -v head="---- $name stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$file" | head -40
}
judge() {
  local label=$1 file=$2 name=$3
  if grep -qF -- "test $name ... ok" "$file"; then
    echo "== $name: passes"
    row "$name $label" passes passes
  elif grep -qF -- "test $name ... FAILED" "$file"; then
    echo "== $name: FAILED"
    block "$name" "$file"
    row "$name $label" passes "NOT: $(block "$name" "$file" | grep -m1 -A1 'panicked at' | tail -1 | cut -c1-160)"
    failed=1
  else
    echo "== $name: not run"
    tail -20 "$file"
    row "$name $label" passes "NOT: not run"
    failed=1
  fi
}
echo "== build at the fix tip, one target at a time"
free -m
( while sleep 15; do echo "mem $(date -u +%T) $(free -m | awk '/^Mem:/ {print "used " $3 " of " $2 ", available " $7}')"; done ) &
sampler=$!
cargo test -p crazyagent --lib --locked --no-run 2>&1 | tail -3
cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -3
kill "$sampler" 2>/dev/null
for unit in sandbox::linux::egress::tests::slirp_reads_and_writes_only_the_files_it_needs sandbox::linux::tests::the_slirp_request_takes_a_process_above_one_and_a_descriptor_above_two; do
  cargo test -p crazyagent --lib --locked -- "$unit" --exact > "$RUNNER_TEMP/unit.txt" 2>&1
  judge "at $fix" "$RUNNER_TEMP/unit.txt" "$unit"
done
bin=$(find target/debug/deps -maxdepth 1 -type f -executable -name 'linux_sandbox-*' -printf '%T@ %p\n' | sort -rn | head -1 | cut -d' ' -f2)
echo "test binary $bin"
echo "== routed rows, slirp4netns watched"
(
  declare -A seen
  while sleep 0.2; do
    for p in $(pgrep -x slirp4netns); do
      [ -n "${seen[$p]:-}" ] && continue
      seen[$p]=1
      echo "slirp4netns $p parent $(ps -o comm= -p "$(ps -o ppid= -p "$p" | tr -d ' ')" 2>/dev/null)"
      echo "  args: $(tr '\0' ' ' < "/proc/$p/cmdline" 2>/dev/null)"
      grep -E '^(Uid|NoNewPrivs|Seccomp|CapPrm|CapEff|CapAmb):' "/proc/$p/status" 2>/dev/null | sed 's/^/  /'
      ls -l "/proc/$p/fd" 2>/dev/null | awk 'NR > 1 {print "  fd " $9 " -> " $11}'
    done
  done
) > "$RUNNER_TEMP/slirp-watch.txt" 2>&1 &
watcher=$!
"$bin" routed > "$RUNNER_TEMP/routed.txt" 2>&1
kill "$watcher" 2>/dev/null
grep -E '^test |^test result' "$RUNNER_TEMP/routed.txt"
for name in $(grep -oE '^test [a-z0-9_]+ \.\.\. ' "$RUNNER_TEMP/routed.txt" | awk '{print $2}'); do
  judge "at $fix" "$RUNNER_TEMP/routed.txt" "$name"
done
echo "== slirp4netns as it ran"
head -80 "$RUNNER_TEMP/slirp-watch.txt"
started=$(grep -c '^slirp4netns ' "$RUNNER_TEMP/slirp-watch.txt")
sandboxed=$(grep -c 'enable-sandbox' "$RUNNER_TEMP/slirp-watch.txt")
by_fd=$(grep -c 'userns-path=/proc/self/fd/' "$RUNNER_TEMP/slirp-watch.txt")
if [ "$started" -gt 0 ] && [ "$sandboxed" -eq 0 ] && [ "$by_fd" -eq "$started" ]; then
  row "slirp4netns seen during the routed rows" "started through --slirp: namespaces by descriptor, no --enable-sandbox" "$started seen, all by descriptor, none with the flag"
else
  row "slirp4netns seen during the routed rows" "started through --slirp: namespaces by descriptor, no --enable-sandbox" "NOT: $started seen, $by_fd by descriptor, $sandboxed with the flag"
  failed=1
fi
echo "== the bridge descriptor row"
"$bin" --exact the_bridge_leaves_no_descriptor_in_the_harness > "$RUNNER_TEMP/bridge.txt" 2>&1
grep -E '^test ' "$RUNNER_TEMP/bridge.txt"
block the_bridge_leaves_no_descriptor_in_the_harness "$RUNNER_TEMP/bridge.txt"
echo "== linux_sandbox in full at $fix"
cargo test -p crazyagent --test linux_sandbox --locked --no-fail-fast > "$RUNNER_TEMP/full.txt" 2>&1
grep -E '^test .* FAILED$|^test result' "$RUNNER_TEMP/full.txt"
for name in $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}'); do
  echo "-- $name"
  block "$name" "$RUNNER_TEMP/full.txt" | grep -A2 'panicked at' | head -4
done
row "linux_sandbox at $fix" "record" "$(grep -E '^test result' "$RUNNER_TEMP/full.txt" | tail -1); failed: $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}' | tr '\n' ' ')"
exit "$failed"
