set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
routed=d73bdc0a8
leak=8aef4a1c2
merge=550804f5c
arm=57dae9fbc
unit=sandbox::linux::egress::tests::slirp_connects_to_no_unix_socket
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
grep -E '^VERSION=' /etc/os-release
/usr/bin/slirp4netns --version 2>&1 | head -1
bwrap --version
git log --oneline -4
tip=$(git rev-parse HEAD)
short() { git rev-parse --short=9 "$1"; }
if [ "$(short HEAD~1)" != "$arm" ] || [ "$(short HEAD~2)" != "$merge" ] || [ "$(short HEAD~2^1)" != "$routed" ] || [ "$(short HEAD~2^2)" != "$leak" ]; then
  row tip "arm $arm, merge $merge of $routed and $leak" "$(short HEAD~1) $(short HEAD~2) $(short HEAD~2^1) $(short HEAD~2^2)"
  exit 1
fi
echo "== descriptors this step holds"
ls -l /proc/self/fd
inherited=$(for f in /proc/self/fd/*; do n=${f##*/}; [ "$n" -gt 2 ] && readlink "$f"; done | grep '^pipe:' | sort -u)
echo "inherited pipes: $inherited"
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
lint() {
  local at=$1
  git checkout -q "$at"
  cargo clippy -p crazyagent --all-targets --locked -- -D warnings > "$RUNNER_TEMP/clippy-$at.txt" 2>&1
  local code=$?
  tail -5 "$RUNNER_TEMP/clippy-$at.txt"
  if [ "$code" -eq 0 ]; then
    row "clippy -p crazyagent --all-targets -D warnings at $at" "clean" "clean"
  else
    grep -E '^(error|warning)' "$RUNNER_TEMP/clippy-$at.txt" | sort | uniq -c | head -20
    grep -E -A6 '^error' "$RUNNER_TEMP/clippy-$at.txt" | head -60
    row "clippy -p crazyagent --all-targets -D warnings at $at" "clean" "NOT: exit $code, $(grep -m1 -E '^error' "$RUNNER_TEMP/clippy-$at.txt" | cut -c1-140)"
    failed=1
  fi
}
( while sleep 15; do echo "mem $(date -u +%T) $(free -m | awk '/^Mem:/ {print "used " $3 " of " $2 ", available " $7}')"; done ) &
sampler=$!
echo "== clippy at the routed tip $routed"
lint "$routed"
echo "== the unix socket row at $routed"
cargo test -p crazyagent --lib --locked --no-run 2>&1 | tail -3
cargo test -p crazyagent --lib --locked -- "$unit" --exact > "$RUNNER_TEMP/unit.txt" 2>&1
judge "at $routed" "$RUNNER_TEMP/unit.txt" "$unit" passes
echo "== clippy at the descriptor tip $leak"
lint "$leak"
echo "== both merged with the arm, at $arm"
git checkout -q "$tip"
cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tail -3
kill "$sampler" 2>/dev/null
bin=$(find target/debug/deps -maxdepth 1 -type f -executable -name 'linux_sandbox-*' -printf '%T@ %p\n' | sort -rn | head -1 | cut -d' ' -f2)
echo "test binary $bin"
(
  declare -A seen
  while sleep 0.2; do
    for p in $(pgrep -x slirp4netns); do
      [ -n "${seen[$p]:-}" ] && continue
      seen[$p]=1
      echo "slirp4netns $p parent $(ps -o comm= -p "$(ps -o ppid= -p "$p" | tr -d ' ')" 2>/dev/null)"
      echo "  args: $(tr '\0' ' ' < "/proc/$p/cmdline" 2>/dev/null)"
      grep -E '^(Uid|NoNewPrivs|Seccomp_filters|CapEff):' "/proc/$p/status" 2>/dev/null | tr '\t' ' ' | sed 's/^/  /'
      ls -l "/proc/$p/fd" 2>/dev/null | awk 'NR > 1 {print "  fd " $9 " -> " $11}'
    done
  done
) > "$RUNNER_TEMP/slirp-watch.txt" 2>&1 &
watcher=$!
"$bin" routed > "$RUNNER_TEMP/routed.txt" 2>&1
kill "$watcher" 2>/dev/null
grep -E '^test |^test result' "$RUNNER_TEMP/routed.txt"
for name in $(grep -oE '^test [a-z0-9_]+ \.\.\. ' "$RUNNER_TEMP/routed.txt" | awk '{print $2}'); do
  judge "at $arm" "$RUNNER_TEMP/routed.txt" "$name" passes
done
for name in the_bridge_leaves_no_descriptor_in_the_harness a_descriptor_the_engine_inherited_reaches_no_harness; do
  "$bin" --exact "$name" > "$RUNNER_TEMP/$name.txt" 2>&1
  judge "at $arm" "$RUNNER_TEMP/$name.txt" "$name" passes
done
echo "== slirp4netns as it ran"
head -80 "$RUNNER_TEMP/slirp-watch.txt"
started=$(grep -c '^slirp4netns ' "$RUNNER_TEMP/slirp-watch.txt")
listed=$(grep -c '^  fd ' "$RUNNER_TEMP/slirp-watch.txt")
held=0
for pipe in $inherited; do
  held=$((held + $(grep -cF -- "-> $pipe" "$RUNNER_TEMP/slirp-watch.txt")))
done
if [ "$started" -gt 0 ] && [ "$listed" -gt 0 ] && [ "$held" -eq 0 ]; then
  row "slirp4netns holds none of the step's inherited pipes" "$started seen, none held" "$started seen, $listed descriptors listed, none held"
else
  row "slirp4netns holds none of the step's inherited pipes" "seen, none held" "NOT: $started seen, $listed listed, $held held"
  failed=1
fi
echo "== linux_sandbox in full at $arm"
cargo test -p crazyagent --test linux_sandbox --locked --no-fail-fast > "$RUNNER_TEMP/full.txt" 2>&1
grep -E '^test .* FAILED$|^test result' "$RUNNER_TEMP/full.txt"
for name in $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}'); do
  echo "-- $name"
  block "$name" "$RUNNER_TEMP/full.txt" | grep -A2 'panicked at' | head -4
done
row "linux_sandbox at $arm" "record" "$(grep -E '^test result' "$RUNNER_TEMP/full.txt" | tail -1); failed: $(grep -E '^test .* FAILED$' "$RUNNER_TEMP/full.txt" | awk '{print $2}' | tr '\n' ' ')"
exit "$failed"
