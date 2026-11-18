set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
tip=50c16e48a
exit3=a_helper_that_cannot_list_its_inherited_descriptors_starts_nothing
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
/usr/bin/slirp4netns --version 2>&1 | head -1
git log --oneline -3
if [ "$(git rev-parse --short=9 HEAD~1)" != "$tip" ] || ! git diff --quiet HEAD~1 HEAD -- . ':!.github'; then
  row tip "HEAD~1 $tip, farm commit only .github" "$(git rev-parse --short=9 HEAD~1)"
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
cargo clippy -p crazyagent --all-targets --locked -- -D warnings > "$RUNNER_TEMP/clippy.txt" 2>&1
code=$?
tail -5 "$RUNNER_TEMP/clippy.txt"
if [ "$code" -eq 0 ]; then
  row "clippy -p crazyagent --all-targets -D warnings at $tip" clean clean
else
  grep -E -A8 '^(error|warning)' "$RUNNER_TEMP/clippy.txt" | head -80 || true
  row "clippy -p crazyagent --all-targets -D warnings at $tip" clean "NOT: exit $code"
  failed=1
fi
cargo test -p crazyagent --lib --locked --no-run 2>&1 | tail -3
cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -3
cargo test -p crazyagent --lib --locked -- sandbox::linux::egress::tests > "$RUNNER_TEMP/egress.txt" 2>&1
grep -E '^test result' "$RUNNER_TEMP/egress.txt" || true
for name in $(grep -oE '^test [a-z0-9_:]+ \.\.\. ' "$RUNNER_TEMP/egress.txt" | awk '{print $2}'); do
  judge "at $tip" "$RUNNER_TEMP/egress.txt" "$name" passes
done
bin=$(find target/debug/deps -maxdepth 1 -type f -executable -name 'linux_sandbox-*' -printf '%T@ %p\n' | sort -rn | head -1 | cut -d' ' -f2)
echo "test binary $bin"
names=$("$bin" --list 2>/dev/null | grep -oE '^[a-z0-9_]*(routed|descriptor)[a-z0-9_]*: test' | cut -d: -f1)
echo "$names"
for name in $names; do
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
judge "at $tip, in the full run" "$RUNNER_TEMP/full.txt" "$exit3" passes
row "linux_sandbox at $tip" "record" "$(grep -E '^test result' "$RUNNER_TEMP/full.txt" | tail -1); failed: $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}' | tr '\n' ' ')"
echo "== mutation: the helper goes on when it cannot list its descriptors"
python3 - <<'PY'
p = "crates/crazyagent/src/sandbox/linux.rs"
t = open(p).read()
old = """    if let Err(reason) = close_inherited(&named) {
        eprintln!("crazyagent-landlock: {reason}");
        return ExitCode::from(3);
    }
"""
assert t.count(old) == 1
open(p, "w").write(t.replace(old, "    let _ = close_inherited(&named);\n"))
PY
git diff --stat
cargo test -p crazyagent --test linux_sandbox --locked -- "$exit3" --exact > "$RUNNER_TEMP/mutated.txt" 2>&1
judge "with the sweep refusal removed" "$RUNNER_TEMP/mutated.txt" "$exit3" fails
git checkout -q -- crates
exit "$failed"
