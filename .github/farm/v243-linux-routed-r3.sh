set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
red=2019fccf8
fix=16f47d587
new=sandbox::linux::egress::tests::slirp_connects_to_no_unix_socket
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
grep -E '^VERSION=' /etc/os-release
/usr/bin/slirp4netns --version 2>&1 | head -1
bwrap --version
git log --oneline -3
tip=$(git rev-parse HEAD)
if [ "$(git rev-parse --short=9 HEAD~1)" != "$fix" ] || [ "$(git rev-parse --short=9 HEAD~2)" != "$red" ]; then
  row tip "HEAD~1 $fix, HEAD~2 $red" "$(git rev-parse --short=9 HEAD~1) $(git rev-parse --short=9 HEAD~2)"
  exit 1
fi
block() {
  local name=$1 file=$2
  awk -v head="---- $name stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$file" | head -40
}
judge() {
  local label=$1 file=$2 name=$3 want=$4
  local got
  if grep -qF -- "test $name ... ok" "$file"; then
    got=passes
  elif grep -qF -- "test $name ... FAILED" "$file"; then
    got=fails
    block "$name" "$file"
  else
    got="not run"
    tail -20 "$file"
  fi
  echo "== $name $label: $got (expected $want)"
  if [ "$got" = "$want" ]; then
    row "$name $label" "$want" "$got"
  else
    row "$name $label" "$want" "NOT: $got $(block "$name" "$file" | grep -m1 -A1 'panicked at' | tail -1 | cut -c1-160)"
    failed=1
  fi
}
( while sleep 15; do echo "mem $(date -u +%T) $(free -m | awk '/^Mem:/ {print "used " $3 " of " $2 ", available " $7}')"; done ) &
sampler=$!
echo "== red at $red: the new row alone, lib built one target at a time"
git checkout -q "$red"
cargo test -p crazyagent --lib --locked --no-run 2>&1 | tail -3
cargo test -p crazyagent --lib --locked -- "$new" --exact > "$RUNNER_TEMP/red.txt" 2>&1
judge "at $red" "$RUNNER_TEMP/red.txt" "$new" fails
if ! grep -qF 'slirp connected to a pathname socket' "$RUNNER_TEMP/red.txt"; then
  row "$new at $red" "fails on the connect" "NOT: another failure"
  failed=1
fi
echo "== fix at $fix, one target at a time"
git checkout -q "$tip"
cargo test -p crazyagent --lib --locked --no-run 2>&1 | tail -3
cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -3
kill "$sampler" 2>/dev/null
cargo test -p crazyagent --lib --locked -- sandbox::linux::egress::tests > "$RUNNER_TEMP/egress.txt" 2>&1
grep -E '^test |^test result' "$RUNNER_TEMP/egress.txt"
for name in $(grep -oE '^test [a-z0-9_:]+ \.\.\. ' "$RUNNER_TEMP/egress.txt" | awk '{print $2}'); do
  judge "at $fix" "$RUNNER_TEMP/egress.txt" "$name" passes
done
cargo test -p crazyagent --lib --locked -- sandbox::linux::tests::the_slirp_request_takes_a_process_above_one_and_a_descriptor_above_two --exact > "$RUNNER_TEMP/parse.txt" 2>&1
judge "at $fix" "$RUNNER_TEMP/parse.txt" sandbox::linux::tests::the_slirp_request_takes_a_process_above_one_and_a_descriptor_above_two passes
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
      grep -E '^(Uid|NoNewPrivs|Seccomp|Seccomp_filters|CapEff):' "/proc/$p/status" 2>/dev/null | sed 's/^/  /'
    done
  done
) > "$RUNNER_TEMP/slirp-watch.txt" 2>&1 &
watcher=$!
"$bin" routed > "$RUNNER_TEMP/routed.txt" 2>&1
kill "$watcher" 2>/dev/null
grep -E '^test |^test result' "$RUNNER_TEMP/routed.txt"
for name in $(grep -oE '^test [a-z0-9_]+ \.\.\. ' "$RUNNER_TEMP/routed.txt" | awk '{print $2}'); do
  if [ "$name" = the_routed_network_leaves_no_descriptor_in_the_harness ]; then
    judge "at $fix (baseline red, V243-LINUX-HARNESS-FD-LEAK)" "$RUNNER_TEMP/routed.txt" "$name" fails
  else
    judge "at $fix" "$RUNNER_TEMP/routed.txt" "$name" passes
  fi
done
echo "== slirp4netns as it ran"
head -60 "$RUNNER_TEMP/slirp-watch.txt"
started=$(grep -c '^slirp4netns ' "$RUNNER_TEMP/slirp-watch.txt")
row "slirp4netns seen during the routed rows" "record" "$started seen; Seccomp_filters: $(grep -oE 'Seccomp_filters:[[:space:]]+[0-9]+' "$RUNNER_TEMP/slirp-watch.txt" | awk '{print $2}' | sort | uniq -c | tr '\n' ' ')"
echo "== linux_sandbox in full at $fix"
cargo test -p crazyagent --test linux_sandbox --locked --no-fail-fast > "$RUNNER_TEMP/full.txt" 2>&1
grep -E '^test .* FAILED$|^test result' "$RUNNER_TEMP/full.txt"
for name in $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}'); do
  echo "-- $name"
  block "$name" "$RUNNER_TEMP/full.txt" | grep -A2 'panicked at' | head -4
done
row "linux_sandbox at $fix" "record" "$(grep -E '^test result' "$RUNNER_TEMP/full.txt" | tail -1); failed: $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}' | tr '\n' ' ')"
exit "$failed"
