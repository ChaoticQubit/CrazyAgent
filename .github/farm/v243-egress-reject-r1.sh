set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
red=542f6a09d
fix=7f7bf16e7
routed=d73bdc0a8
merge=1f4ad731e
unit=sandbox::linux::egress::tests::a_refused_packet_is_answered_so_a_refused_connect_fails_at_once
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
grep -E '^VERSION=' /etc/os-release
/usr/bin/slirp4netns --version 2>&1 | head -1
iptables --version
git log --oneline -3
tip=$(git rev-parse HEAD)
short() { git rev-parse --short=9 "$1"; }
if [ "$(short HEAD~1)" != "$merge" ] || [ "$(short HEAD~1^1)" != "$fix" ] || [ "$(short HEAD~1^2)" != "$routed" ] || [ "$(short HEAD~1^1~1)" != "$red" ]; then
  row tip "merge $merge of $fix and $routed, red $red" "$(short HEAD~1) $(short HEAD~1^1) $(short HEAD~1^2) $(short HEAD~1^1~1)"
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
echo "== red at $red: the rules row alone, lib only"
git checkout -q "$red"
cargo test -p crazyagent --lib --locked --no-run 2>&1 | tail -3
cargo test -p crazyagent --lib --locked -- "$unit" --exact > "$RUNNER_TEMP/red.txt" 2>&1
judge "at $red" "$RUNNER_TEMP/red.txt" "$unit" fails
echo "== the fix merged with routed, at $merge"
git checkout -q "$tip"
cargo clippy -p crazyagent --all-targets --locked -- -D warnings > "$RUNNER_TEMP/clippy.txt" 2>&1
code=$?
tail -5 "$RUNNER_TEMP/clippy.txt"
if [ "$code" -eq 0 ]; then
  row "clippy -p crazyagent --all-targets -D warnings at $merge" clean clean
else
  grep -E -A6 '^error' "$RUNNER_TEMP/clippy.txt" | head -60
  row "clippy -p crazyagent --all-targets -D warnings at $merge" clean "NOT: exit $code, $(grep -m1 -E '^error' "$RUNNER_TEMP/clippy.txt" | cut -c1-140)"
  failed=1
fi
cargo test -p crazyagent --lib --locked --no-run 2>&1 | tail -3
cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -3
kill "$sampler" 2>/dev/null
cargo test -p crazyagent --lib --locked -- sandbox::linux::egress::tests > "$RUNNER_TEMP/egress.txt" 2>&1
grep -E '^test result' "$RUNNER_TEMP/egress.txt"
for name in $(grep -oE '^test [a-z0-9_:]+ \.\.\. ' "$RUNNER_TEMP/egress.txt" | awk '{print $2}'); do
  judge "at $merge" "$RUNNER_TEMP/egress.txt" "$name" passes
done
bin=$(find target/debug/deps -maxdepth 1 -type f -executable -name 'linux_sandbox-*' -printf '%T@ %p\n' | sort -rn | head -1 | cut -d' ' -f2)
echo "test binary $bin"
echo "== routed rows, each timed"
for name in $("$bin" --list 2>/dev/null | grep -oE '^[a-z0-9_]*routed[a-z0-9_]*: test' | cut -d: -f1); do
  started=$(date +%s)
  "$bin" --exact "$name" > "$RUNNER_TEMP/$name.txt" 2>&1
  took=$(( $(date +%s) - started ))
  echo "$name took ${took}s"
  if [ "$name" = the_routed_network_leaves_no_descriptor_in_the_harness ]; then
    judge "at $merge in ${took}s (needs V243-LINUX-HARNESS-FD-LEAK)" "$RUNNER_TEMP/$name.txt" "$name" fails
  else
    judge "at $merge in ${took}s" "$RUNNER_TEMP/$name.txt" "$name" passes
  fi
done
echo "== linux_sandbox in full at $merge"
cargo test -p crazyagent --test linux_sandbox --locked --no-fail-fast > "$RUNNER_TEMP/full.txt" 2>&1
grep -E '^test .* FAILED$|^test result' "$RUNNER_TEMP/full.txt"
for name in $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}'); do
  echo "-- $name"
  block "$name" "$RUNNER_TEMP/full.txt" | grep -A2 'panicked at' | head -4
done
row "linux_sandbox at $merge" "record" "$(grep -E '^test result' "$RUNNER_TEMP/full.txt" | tail -1); failed: $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}' | tr '\n' ' ')"
exit "$failed"
