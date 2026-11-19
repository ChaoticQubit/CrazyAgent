set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
tip=6f17c0d76
docs=aabdc20a3
prev=3e4f748e8
box=crates/crazyagent/tests/linux_sandbox.rs
ROW=a_hidden_folders_cover_stays_on_whatever_the_agent_unmounts
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"
  printf 'row: %s | expected %s | %s\n' "$1" "$2" "$3"
}
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
git log --oneline -4
code_tip=$(git log -1 --format=%H HEAD -- . ':!.github')
chain="$(git log --format=%h --abbrev=9 -3 "$code_tip" | tr '\n' ' ')"
if [ "$chain" != "$tip $docs $prev " ]; then
  row chain "$tip $docs $prev" "NOT: $chain"
  exit 1
fi
code_lines=$(git diff -U0 "$prev" "$docs" | grep -E '^[+-]' | grep -vE '^(\+\+\+|---)' | grep -cvE '^[+-][[:space:]]*///')
row "$prev..$docs changes docstring lines only" 0 "$code_lines"
[ "$code_lines" = 0 ] || failed=1
box_files="$(git diff --name-only "$docs" "$tip" | tr '\n' ' ')"
if [ "$box_files" != "$box " ]; then
  row "$docs..$tip changes only $box" "$box" "NOT: $box_files"
  exit 1
fi
bwrap=$(command -v bwrap)
"$bwrap" --version
if "$bwrap" --die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts --dev-bind / / --proc /proc --unshare-net -- /bin/true; then
  row "this runner's bubblewrap nests (the Nested legs run)" yes yes
else
  row "this runner's bubblewrap nests (the Nested legs run)" yes "NOT: no"
  failed=1
fi
command -v unshare
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
  else
    got="not run"
    tail -20 "$file"
  fi
  echo "== $name $label: $got (expected $want)"
  [ "$got" = fails ] && block "$name" "$file"
  if [ "$got" = "$want" ]; then
    row "$name $label" "$want" "$got"
  else
    row "$name $label" "$want" "NOT: $got $(block "$name" "$file" | grep -m1 -A3 'panicked at' | tail -3 | tr '\n' ' ' | cut -c1-400)"
    failed=1
  fi
}
whole() {
  local label=$1 file=$2
  local reds
  reds=$(grep -E '^test .* FAILED$' "$file" | awk '{print $2}' | tr '\n' ' ')
  for name in $reds; do
    echo "-- $name"
    block "$name" "$file" | grep -A4 'panicked at' | head -6
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
refused() {
  local at=$1
  git checkout -q -f "$at"
  sed -i 's/unshare -Urm sh -c/false -Urm sh -c/' "$box"
  local count
  count=$(grep -c 'false -Urm sh -c' "$box")
  echo "== $ROW at $at with the inner unshare refused ($count replaced)"
  git diff --stat
  [ "$count" = 1 ] || { row "one unshare replaced at $at" 1 "NOT: $count"; failed=1; }
  cargo test -p crazyagent --test linux_sandbox --locked -- --exact "$ROW" > "$RUNNER_TEMP/refused-$at.txt" 2>&1
  grep -E '^(running [0-9]+ tests?|test result)' "$RUNNER_TEMP/refused-$at.txt"
}
refused "$prev"
judge "at $prev with unshare refused (the old row reads the same)" "$RUNNER_TEMP/refused-$prev.txt" "$ROW" passes
refused "$tip"
judge "at $tip with unshare refused" "$RUNNER_TEMP/refused-$tip.txt" "$ROW" fails
echo "== green leg at $tip"
git checkout -q -f "$code_tip"
git status --short
cargo test -p crazyagent --test linux_sandbox --locked > "$RUNNER_TEMP/tip-box.txt" 2>&1
grep -E '^(running [0-9]+ tests?|test result)' "$RUNNER_TEMP/tip-box.txt"
judge "at $tip" "$RUNNER_TEMP/tip-box.txt" "$ROW" passes
whole "linux_sandbox at $tip" "$RUNNER_TEMP/tip-box.txt"
cargo test -p crazyagent --test linux_hidden_mounts --locked -- --ignored --test-threads=1 > "$RUNNER_TEMP/tip-mounts.txt" 2>&1
grep -E '^(running [0-9]+ tests?|test result)' "$RUNNER_TEMP/tip-mounts.txt"
whole "linux_hidden_mounts --ignored at $tip" "$RUNNER_TEMP/tip-mounts.txt"
echo "max_user_namespaces at the end $(cat /proc/sys/user/max_user_namespaces)"
exit "$failed"
