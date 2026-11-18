set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
tip=c059ae6b4
base=410bb735e
timed=a_refused_connect_from_a_routed_agent_fails_at_once
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
grep -E '^VERSION=' /etc/os-release
echo "lsm: $(cat /sys/kernel/security/lsm 2>&1)"
/usr/bin/slirp4netns --version 2>&1 | head -1
iptables --version
bwrap --version
git log --oneline -3
if [ "$(git rev-parse --short=9 HEAD~1)" != "$tip" ] || [ "$(git rev-parse --short=9 HEAD~2)" != "$base" ] || ! git diff --quiet HEAD~1 HEAD -- . ':!.github'; then
  row tip "HEAD~1 $tip on $base, farm commit only .github" "$(git rev-parse --short=9 HEAD~1) on $(git rev-parse --short=9 HEAD~2)"
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
newest() { find target/debug/deps -maxdepth 1 -type f -executable -name 'linux_sandbox-*' -printf '%T@ %p\n' | sort -rn | head -1 | cut -d' ' -f2; }
timed_run() {
  local label=$1 want=$2 bin started took
  cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -2
  bin=$(newest)
  started=$(date +%s)
  "$bin" --exact "$timed" > "$RUNNER_TEMP/timed.txt" 2>&1
  took=$(( $(date +%s) - started ))
  judge "$label in ${took}s" "$RUNNER_TEMP/timed.txt" "$timed" "$want"
}
echo "== clippy at $tip"
cargo clippy -p crazyagent --all-targets --locked -- -D warnings > "$RUNNER_TEMP/clippy.txt" 2>&1
code=$?
tail -3 "$RUNNER_TEMP/clippy.txt"
if [ "$code" -eq 0 ]; then
  row "clippy -p crazyagent --all-targets -D warnings at $tip" clean clean
else
  grep -E -A8 '^(error|warning)' "$RUNNER_TEMP/clippy.txt" | head -60 || true
  row "clippy -p crazyagent --all-targets -D warnings at $tip" clean "NOT: exit $code"
  failed=1
fi
cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -2
echo "== Landlock network rules on this kernel"
target/debug/crazyagent-landlock --probe --limit-network
probe=$?
echo "probe --limit-network exit $probe"
if [ "$probe" -eq 0 ]; then
  row "helper --probe --limit-network (Landlock network rules hold)" "exit 0" "exit 0"
else
  row "helper --probe --limit-network (Landlock network rules hold)" "exit 0" "NOT: exit $probe"
  failed=1
fi
bin=$(newest)
echo "== routed rows at $tip, each timed"
for name in $("$bin" --list 2>/dev/null | grep -oE '^[a-z0-9_]*routed[a-z0-9_]*: test' | cut -d: -f1); do
  started=$(date +%s)
  "$bin" --exact "$name" > "$RUNNER_TEMP/$name.txt" 2>&1
  took=$(( $(date +%s) - started ))
  judge "at $tip in ${took}s" "$RUNNER_TEMP/$name.txt" "$name" passes
done
echo "== linux_sandbox in full at $tip"
cargo test -p crazyagent --test linux_sandbox --locked --no-fail-fast > "$RUNNER_TEMP/full.txt" 2>&1
grep -E '^test .* FAILED$|^test result' "$RUNNER_TEMP/full.txt" || true
for name in $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}'); do
  echo "-- $name"
  block "$name" "$RUNNER_TEMP/full.txt" | grep -A2 'panicked at' | head -4
done
row "linux_sandbox at $tip" "record" "$(grep -E '^test result' "$RUNNER_TEMP/full.txt" | tail -1); failed: $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}' | tr '\n' ' ')"
echo "== the REJECT fix 7f7bf16e7 reverted: the egress rules DROP again"
git apply -R .github/farm/v243-reject-fix.diff || { row "revert 7f7bf16e7" applies "NOT: does not apply"; exit 1; }
grep -c ' -j DROP")' crates/crazyagent/src/sandbox/linux/egress.rs || true
timed_run "at $tip with DROP rules (new row)" fails
echo "== the old row from $base, DROP rules"
git checkout -q "$base" -- crates/crazyagent/tests/linux_sandbox.rs
timed_run "at $base with DROP rules (old row, V243-EGRESS-TIMED-ROW-LANDLOCK)" passes
git checkout -q HEAD -- crates
exit "$failed"
