set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
tip=2cf0f3e28
call=8cc5aefb6
legs=508e84877
prev=6f17c0d76
box=crates/crazyagent/tests/linux_sandbox.rs
ROW=a_hidden_folders_cover_stays_on_whatever_the_agent_unmounts
OUT=a_hidden_folder_is_out_of_reach_on_both_bubblewrap_backends
INSTALLED=an_installed_sandbox_with_a_hidden_folder_keeps_it_out_or_refuses_it
ROWS="$ROW $OUT $INSTALLED"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"
  printf 'row: %s | expected %s | %s\n' "$1" "$2" "$3"
}
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
git log --oneline -3
code_tip=$(git log -1 --format=%H HEAD -- . ':!.github')
chain="$(git log --format=%h --abbrev=9 -4 "$code_tip" | tr '\n' ' ')"
if [ "$chain" != "$tip $call $legs $prev " ]; then
  row chain "$tip $call $legs $prev" "NOT: $chain"
  exit 1
fi
box_files="$(git diff --name-only "$prev" "$tip" | tr '\n' ' ')"
if [ "$box_files" != "$box " ]; then
  row "$prev..$tip changes only $box" "$box" "NOT: $box_files"
  exit 1
fi
bwrap=$(command -v bwrap)
"$bwrap" --version
unshare --version
if "$bwrap" --die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts --dev-bind / / --proc /proc --unshare-net -- /bin/true; then
  row "this runner's bubblewrap nests (the Nested legs run)" yes yes
else
  row "this runner's bubblewrap nests (the Nested legs run)" yes "NOT: no"
  failed=1
fi
block() {
  local name=$1 file=$2 lines=$3
  awk -v head="---- $name stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$file" | head -"$lines"
}
judge() {
  local label=$1 file=$2 name=$3 want=$4
  local got
  if grep -qF -- "test $name ... ok" "$file"; then
    got=passes
  elif grep -qF -- "test $name ... FAILED" "$file"; then
    got=fails
  else
    got="not run"
    tail -20 "$file"
  fi
  echo "== $name $label: $got (expected $want)"
  [ "$got" = fails ] && block "$name" "$file" 40
  if [ "$got" = "$want" ]; then
    row "$name $label" "$want" "$got"
  else
    row "$name $label" "$want" "NOT: $got $(block "$name" "$file" 40 | grep -m1 -A3 'panicked at' | tail -3 | tr '\n' ' ' | cut -c1-400)"
    failed=1
  fi
}
whole() {
  local label=$1 file=$2
  local reds
  reds=$(grep -E '^test .* FAILED$' "$file" | awk '{print $2}' | tr '\n' ' ')
  for name in $reds; do
    echo "-- $name"
    block "$name" "$file" 40 | grep -A4 'panicked at' | head -6
  done
  row "$label" record "$(grep -E '^test result' "$file" | tail -1)"
  if [ -n "$reds" ]; then
    row "$label reds" none "NOT: $reds"
    failed=1
  elif ! grep -qE '^test result: ok' "$file"; then
    row "$label reds" none "NOT: no ok result"
    failed=1
  else
    row "$label reds" none none
  fi
}
swap() {
  local label=$1 old=$2 new=$3 want=$4
  python3 -I - "$box" "$old" "$new" "$want" <<'PY'
import sys
path, old, new, want = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
text = open(path).read()
count = text.count(old)
print(f"replaced {count} of {old!r}")
if count != want:
    sys.exit(1)
open(path, "w").write(text.replace(old, new))
PY
  local rc=$?
  if [ "$rc" = 0 ]; then
    row "$label: $want replaced" "$want" "$want"
  else
    row "$label: $want replaced" "$want" "NOT: count differs"
    failed=1
  fi
  return "$rc"
}
leg() {
  local label=$1 at=$2
  shift 2
  git checkout -q -f "$at"
  echo "== $label at $at"
  while [ "$#" -ge 3 ]; do
    swap "$label" "$1" "$2" "$3" || return 1
    shift 3
  done
  git diff --stat
  cargo test -p crazyagent --test linux_sandbox --locked -- --exact $ROWS > "$RUNNER_TEMP/$label.txt" 2>&1
  grep -E '^(running [0-9]+ tests?|test result)' "$RUNNER_TEMP/$label.txt"
}
if leg old-leg-silenced "$prev" "unshare -Urm sh -c" "false -Urm sh -c" 2; then
  for name in $OUT $INSTALLED; do
    judge "at $prev with both unshare -Urm legs silenced (the old leg reads the same)" "$RUNNER_TEMP/old-leg-silenced.txt" "$name" passes
  done
fi
if leg mapped-silenced "$tip" '"unshare -Urm"' '"false -Urm"' 1; then
  for name in $ROWS; do
    judge "at $tip with the mapped leg silenced" "$RUNNER_TEMP/mapped-silenced.txt" "$name" fails
  done
fi
if leg unmapped-silenced "$tip" '"unshare -Um --propagation unchanged"' '"false -Um --propagation unchanged"' 1; then
  for name in $ROWS; do
    judge "at $tip with the unmapped leg silenced" "$RUNNER_TEMP/unmapped-silenced.txt" "$name" fails
  done
fi
if leg call-silenced "$tip" '"echo umount2 $(python3 -I -c' '"echo umount2 $(false -I -c' 1; then
  for name in $ROWS; do
    judge "at $tip with the umount2 call silenced" "$RUNNER_TEMP/call-silenced.txt" "$name" fails
  done
fi
if leg call-unmounts "$tip" 'c.umount2(p, f) and' '0 and' 1; then
  for name in $ROWS; do
    judge "at $tip with the umount2 call reading as an unmount" "$RUNNER_TEMP/call-unmounts.txt" "$name" fails
  done
fi
if leg held-silenced "$tip" "{legs}python3 -I -c '{HELD}'" "{legs}false -I -c '{HELD}'" 1; then
  for name in $ROWS; do
    judge "at $tip with the held leg silenced" "$RUNNER_TEMP/held-silenced.txt" "$name" fails
  done
fi
if leg held-unmounts "$tip" 'name(c.umount2(p, f))' 'name(0)' 1; then
  for name in $ROWS; do
    judge "at $tip with the held umount2 reading as an unmount" "$RUNNER_TEMP/held-unmounts.txt" "$name" fails
  done
fi
if leg held-capless "$tip" 'c.unshare(0x10000000 | 0x00020000)' 'c.unshare(0)' 1; then
  for name in $ROWS; do
    judge "at $tip with the held leg making no namespace (no capability)" "$RUNNER_TEMP/held-capless.txt" "$name" fails
  done
fi
if leg read "$tip" \
  'without_namespace_legs(backend, &stdout).is_some(),' 'false,' 1 \
  'if without_namespace_legs(backend, &stdout).as_deref() != Some("1\ndone\n") {' 'if true {' 1; then
  for name in $ROWS; do
    echo "== read: $name at $tip, every leg line shown"
    block "$name" "$RUNNER_TEMP/read.txt" 80
  done
fi
held_lines="$(grep -ohE 'inner held|capeff [0-9a-f]+|umount2 [A-Z0-9]+ [A-Z0-9]+' "$RUNNER_TEMP/read.txt" | sort | uniq -c | tr '\n' ';')"
echo "== held and umount2 lines in the read: $held_lines"
if grep -qE 'umount2 ([A-Z0-9]+ )?0([^0-9a-zA-Z]|$)' "$RUNNER_TEMP/read.txt"; then
  row "PRODUCT FINDING: an agent umount2 returned 0 (cover removable)" "no 0" "NOT: $(grep -oE 'umount2 [A-Z0-9]+ [A-Z0-9]+' "$RUNNER_TEMP/read.txt" | sort -u | tr '\n' ' ')"
  failed=1
else
  row "no agent umount2 returned 0 in the read" "no 0" "none"
fi
echo "== green leg at $tip"
git checkout -q -f "$code_tip"
git status --short
cargo test -p crazyagent --test linux_sandbox --locked > "$RUNNER_TEMP/tip-box.txt" 2>&1
grep -E '^(running [0-9]+ tests?|test result)' "$RUNNER_TEMP/tip-box.txt"
for name in $ROWS; do
  judge "at $tip" "$RUNNER_TEMP/tip-box.txt" "$name" passes
done
whole "linux_sandbox at $tip" "$RUNNER_TEMP/tip-box.txt"
cargo test -p crazyagent --test linux_hidden_mounts --locked -- --ignored --test-threads=1 > "$RUNNER_TEMP/tip-mounts.txt" 2>&1
grep -E '^(running [0-9]+ tests?|test result)' "$RUNNER_TEMP/tip-mounts.txt"
whole "linux_hidden_mounts --ignored at $tip" "$RUNNER_TEMP/tip-mounts.txt"
for set in --lib --bins --tests; do
  CARGO_BUILD_JOBS=2 cargo clippy -p crazyagent "$set" --locked -- -D warnings > "$RUNNER_TEMP/clippy$set.txt" 2>&1
  rc=$?
  grep -E '^(warning|error)' "$RUNNER_TEMP/clippy$set.txt" | sort | uniq -c | head -20
  row "clippy -p crazyagent $set -D warnings at $tip" "rc 0" "rc $rc"
  [ "$rc" = 0 ] || { tail -30 "$RUNNER_TEMP/clippy$set.txt"; failed=1; }
done
echo "max_user_namespaces at the end $(cat /proc/sys/user/max_user_namespaces)"
exit "$failed"
