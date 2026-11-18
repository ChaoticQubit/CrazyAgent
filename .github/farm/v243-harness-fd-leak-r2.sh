set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
merge=1699e0887
uname -r
git log --oneline -3
[ "$(git rev-parse --short=9 HEAD~1)" = "$merge" ] || { echo "HEAD~1 is not $merge"; exit 1; }
block() {
  local name=$1 file=$2
  awk -v head="---- $name stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$file" | head -40
}
cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -3
bin=$(find target/debug/deps -maxdepth 1 -type f -executable -name 'linux_sandbox-*' -printf '%T@ %p\n' | sort -rn | head -1 | cut -d' ' -f2)
echo "test binary $bin"
names=$("$bin" --list 2>/dev/null | grep -oE '^[a-z0-9_]*(routed|descriptor)[a-z0-9_]*: test' | cut -d: -f1)
echo "$names"
[ -n "$names" ] || { echo "no rows listed"; exit 1; }
for name in $names; do
  started=$(date +%s)
  "$bin" --exact "$name" > "$RUNNER_TEMP/$name.txt" 2>&1
  took=$(( $(date +%s) - started ))
  if grep -qF -- "test $name ... ok" "$RUNNER_TEMP/$name.txt"; then
    got=passes
  else
    got=fails
    failed=1
    block "$name" "$RUNNER_TEMP/$name.txt"
    tail -15 "$RUNNER_TEMP/$name.txt"
  fi
  echo "== $name: $got in ${took}s"
  printf '| %s at %s | passes | %s in %ss |\n' "$name" "$merge" "$got" "$took" >> "$summary"
done
exit "$failed"
