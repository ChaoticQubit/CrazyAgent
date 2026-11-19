set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
red=9e43cfc93
base=c8a67418f
reds="a_mount_made_after_the_start_inside_the_project_stays_out_of_both_bubblewrap_backends a_mount_made_after_the_start_outside_every_grant_stays_out_of_a_nested_agent"
kept="a_second_mount_outside_every_grant_refuses_a_nested_launch a_second_mount_inside_the_project_refuses_the_launch_on_both_bubblewrap_backends"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"
  printf 'row: %s | expected %s | %s\n' "$1" "$2" "$3"
}
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
git log --oneline -4
code_tip=$(git log -1 --format=%H HEAD -- . ':!.github')
chain="$(git log --first-parent --format=%h --abbrev=9 -2 "$code_tip" | tr '\n' ' ')"
if [ "$chain" != "$red $base " ]; then
  row chain "$red $base" "NOT: $chain"
  exit 1
fi
bwrap=$(command -v bwrap)
"$bwrap" --version
if "$bwrap" --die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts --dev-bind / / --proc /proc --unshare-net -- /bin/true; then
  row "this runner's bubblewrap nests" yes yes
else
  row "this runner's bubblewrap nests" yes "NOT: no"
  failed=1
fi
if sudo -n true; then
  row "passwordless sudo" yes yes
else
  row "passwordless sudo" yes "NOT: no"
  failed=1
fi
block() {
  local name=$1 file=$2
  awk -v head="---- $name stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$file" | head -40
}
judge() {
  local name=$1 file=$2 want=$3
  local got
  if grep -qF -- "test $name ... ok" "$file"; then
    got=passes
  elif grep -qF -- "test $name ... FAILED" "$file"; then
    got=fails
  else
    got="not run"
    tail -20 "$file"
  fi
  echo "== $name at $red: $got (expected $want)"
  [ "$got" = fails ] && block "$name" "$file"
  if [ "$got" = "$want" ]; then
    row "$name at $red" "$want" "$got"
  else
    row "$name at $red" "$want" "NOT: $got"
    failed=1
  fi
}
cargo test -p crazyagent --test linux_hidden_mounts --locked -- --ignored --test-threads=1 > "$RUNNER_TEMP/red-mounts.txt" 2>&1
grep -E '^test result' "$RUNNER_TEMP/red-mounts.txt"
for name in $reds; do
  judge "$name" "$RUNNER_TEMP/red-mounts.txt" fails
done
for name in $kept; do
  judge "$name" "$RUNNER_TEMP/red-mounts.txt" passes
done
exit "$failed"
