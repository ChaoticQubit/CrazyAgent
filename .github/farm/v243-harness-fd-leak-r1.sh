set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
red=503d93c41
fix=8aef4a1c2
new=a_descriptor_the_engine_inherited_reaches_no_harness
bridge=the_bridge_leaves_no_descriptor_in_the_harness
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
grep -E '^VERSION=' /etc/os-release
bwrap --version
git log --oneline -3
echo "== descriptors this step holds"
ls -l /proc/self/fd
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
binary() {
  find target/debug/deps -maxdepth 1 -type f -executable -name 'linux_sandbox-*' -printf '%T@ %p\n' | sort -rn | head -1 | cut -d' ' -f2
}
( while sleep 15; do echo "mem $(date -u +%T) $(free -m | awk '/^Mem:/ {print "used " $3 " of " $2 ", available " $7}')"; done ) &
sampler=$!
echo "== red at $red: linux_sandbox alone"
git checkout -q "$red"
cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -3
bin=$(binary)
"$bin" --exact "$new" > "$RUNNER_TEMP/red.txt" 2>&1
judge "at $red" "$RUNNER_TEMP/red.txt" "$new" fails
"$bin" --exact "$bridge" > "$RUNNER_TEMP/red-bridge.txt" 2>&1
judge "at $red (baseline, the runner's pipes)" "$RUNNER_TEMP/red-bridge.txt" "$bridge" fails
echo "== fix at $fix"
git checkout -q "$tip"
cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -3
kill "$sampler" 2>/dev/null
bin=$(binary)
"$bin" --exact "$new" > "$RUNNER_TEMP/fix.txt" 2>&1
judge "at $fix" "$RUNNER_TEMP/fix.txt" "$new" passes
"$bin" --exact "$bridge" > "$RUNNER_TEMP/fix-bridge.txt" 2>&1
judge "at $fix" "$RUNNER_TEMP/fix-bridge.txt" "$bridge" passes
echo "== linux_sandbox in full at $fix"
cargo test -p crazyagent --test linux_sandbox --locked --no-fail-fast > "$RUNNER_TEMP/full.txt" 2>&1
grep -E '^test .* FAILED$|^test result' "$RUNNER_TEMP/full.txt"
for name in $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}'); do
  echo "-- $name"
  block "$name" "$RUNNER_TEMP/full.txt" | grep -A2 'panicked at' | head -4
done
row "linux_sandbox at $fix" "record" "$(grep -E '^test result' "$RUNNER_TEMP/full.txt" | tail -1); failed: $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}' | tr '\n' ' ')"
exit "$failed"
