set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
tip=3e4f748e8
carried=c95b4d3a9
fix=8c4f8e7ca
red=1a81409ce
red1=9e43cfc93
base=c8a67418f
only=crates/crazyagent/tests/linux_hidden_mounts.rs
mount_reds="a_mount_made_after_the_start_inside_the_project_stays_out_of_both_bubblewrap_backends a_mount_made_after_the_start_outside_every_grant_stays_out_of_a_nested_agent the_helper_and_both_builds_fail_closed_where_no_user_namespace_can_be_made"
mount_kept="a_second_mount_outside_every_grant_refuses_a_nested_launch a_second_mount_inside_the_project_refuses_the_launch_on_both_bubblewrap_backends"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"
  printf 'row: %s | expected %s | %s\n' "$1" "$2" "$3"
}
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
git log --oneline -7
code_tip=$(git log -1 --format=%H HEAD -- . ':!.github')
chain="$(git log --format=%h --abbrev=9 -6 "$code_tip" | tr '\n' ' ')"
if [ "$chain" != "$tip $carried $fix $red $red1 $base " ]; then
  row chain "$tip $carried $fix $red $red1 $base" "NOT: $chain"
  exit 1
fi
changed="$(git diff --name-only "$carried" "$code_tip" | tr '\n' ' ')"
if [ "$changed" != "$only " ]; then
  row "only $only changed since $carried" "$only" "NOT: $changed"
  exit 1
fi
row "only $only changed since $carried (lib and linux_sandbox greens carry from run 44449273025)" yes yes
bwrap=$(command -v bwrap)
"$bwrap" --version
if "$bwrap" --die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts --dev-bind / / --proc /proc --unshare-net -- /bin/true; then
  row "this runner's bubblewrap nests (the Nested legs run)" yes yes
else
  row "this runner's bubblewrap nests (the Nested legs run)" yes "NOT: no"
  failed=1
fi
if sudo -n true; then
  row "passwordless sudo for the bind mount and sysctl rows" yes yes
else
  row "passwordless sudo for the bind mount and sysctl rows" yes "NOT: no"
  failed=1
fi
echo "max_user_namespaces $(cat /proc/sys/user/max_user_namespaces)"
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
results() {
  grep -E '^(running [0-9]+ tests?|test result)' "$1"
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
echo "== red leg: code at $red, $only from $tip"
git checkout -q "$red"
git checkout -q "$tip" -- "$only"
git status --short
cargo test -p crazyagent --test linux_hidden_mounts --locked -- --ignored --test-threads=1 > "$RUNNER_TEMP/red-mounts.txt" 2>&1
results "$RUNNER_TEMP/red-mounts.txt"
for name in $mount_reds; do
  judge "at $red" "$RUNNER_TEMP/red-mounts.txt" "$name" fails
done
for name in $mount_kept; do
  judge "at $red" "$RUNNER_TEMP/red-mounts.txt" "$name" passes
done
echo "max_user_namespaces after the red leg $(cat /proc/sys/user/max_user_namespaces)"
echo "== green leg at $tip"
git checkout -q -f "$code_tip"
git status --short
cargo test -p crazyagent --test linux_hidden_mounts --locked -- --ignored --test-threads=1 > "$RUNNER_TEMP/tip-mounts.txt" 2>&1
results "$RUNNER_TEMP/tip-mounts.txt"
for name in $mount_reds $mount_kept; do
  judge "at $tip" "$RUNNER_TEMP/tip-mounts.txt" "$name" passes
done
whole "linux_hidden_mounts --ignored at $tip" "$RUNNER_TEMP/tip-mounts.txt"
echo "max_user_namespaces at the end $(cat /proc/sys/user/max_user_namespaces)"
exit "$failed"
