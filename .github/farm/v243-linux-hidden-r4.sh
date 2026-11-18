set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
tip=705be2541
red=64c13f83e
red2=24232b3c9
red1=77844d849
base=557cebe05
lib_reds="sandbox::linux::tests::a_second_mount_of_a_hidden_folders_disk_refuses_the_launch sandbox::linux_sockets_tests::the_hidden_folder_refusals_have_their_own_codes_and_read_plainly"
box_reds="an_absent_hidden_folder_cannot_be_made_on_both_bubblewrap_backends a_hidden_folder_made_during_the_run_stays_out_of_reach_on_both_bubblewrap_backends"
mount_reds="a_second_mount_outside_every_grant_refuses_a_nested_launch a_second_mount_inside_the_project_refuses_the_launch_on_both_bubblewrap_backends"
lib_kept="engine::owner::transport::master_host_launch::tests::master_sandbox_hides_a_listed_folder_from_a_real_child_on_linux sandbox::linux::tests::a_nest_takes_exact_read_only_empty_folder_pairs_before_its_working_folder sandbox::linux_sockets_tests::the_strict_refusal_has_its_own_code_and_reads_plainly"
helper=target/debug/crazyagent-landlock
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
git log --oneline -6
code_tip=$(git log -1 --format=%H HEAD -- . ':!.github')
chain="$(git log --format=%h --abbrev=9 -5 "$code_tip" | tr '\n' ' ')"
if [ "$chain" != "$tip $red $red2 $red1 $base " ]; then
  row tip "$tip $red $red2 $red1 $base" "NOT: $chain"
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
if sudo -n true; then
  row "passwordless sudo for the bind mount rows" yes yes
else
  row "passwordless sudo for the bind mount rows" yes "NOT: no"
  failed=1
fi
grep -c '' /proc/self/mountinfo
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
lib_exe() {
  local at=$1 exe
  exe=$(cargo test -p crazyagent --lib --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-$at.txt" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))$/\1/p' | head -1)
  tail -2 "$RUNNER_TEMP/build-$at.txt" >&2
  if [ -z "$exe" ] || [ ! -x "$exe" ]; then
    row "lib test binary at $at" built "NOT: none"
    exit 1
  fi
  realpath "$exe"
}
run_lib() {
  local exe=$1
  shift
  (cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" "$exe" "$@")
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
echo "== red leg at $red"
git checkout -q "$red"
exe=$(lib_exe "$red")
run_lib "$exe" $lib_reds > "$RUNNER_TEMP/red-lib.txt" 2>&1
for name in $lib_reds; do
  judge "at $red" "$RUNNER_TEMP/red-lib.txt" "$name" fails
done
cargo test -p crazyagent --test linux_sandbox --locked -- --exact $box_reds > "$RUNNER_TEMP/red-box.txt" 2>&1
for name in $box_reds; do
  judge "at $red" "$RUNNER_TEMP/red-box.txt" "$name" fails
done
cargo test -p crazyagent --test linux_hidden_mounts --locked -- --ignored --test-threads=1 > "$RUNNER_TEMP/red-mounts.txt" 2>&1
for name in $mount_reds; do
  judge "at $red" "$RUNNER_TEMP/red-mounts.txt" "$name" fails
done
echo "== green leg at $tip"
git checkout -q "$code_tip"
cargo build -p crazyagent --bin crazyagent-landlock --locked 2>&1 | tail -1
if [ -x "$helper" ]; then
  row "helper built at $tip" built "built $(sha256sum "$helper" | cut -c1-12)"
else
  row "helper built at $tip" built "NOT: missing"
  failed=1
fi
exe=$(lib_exe "$tip")
run_lib "$exe" sandbox:: master_host_launch > "$RUNNER_TEMP/tip-lib.txt" 2>&1
for name in $lib_reds $lib_kept; do
  judge "at $tip" "$RUNNER_TEMP/tip-lib.txt" "$name" passes
done
whole "lib sandbox:: master_host_launch at $tip" "$RUNNER_TEMP/tip-lib.txt"
cargo test -p crazyagent --test linux_sandbox --locked > "$RUNNER_TEMP/tip-box.txt" 2>&1
for name in $box_reds; do
  judge "at $tip" "$RUNNER_TEMP/tip-box.txt" "$name" passes
done
whole "linux_sandbox at $tip" "$RUNNER_TEMP/tip-box.txt"
cargo test -p crazyagent --test linux_hidden_mounts --locked -- --ignored --test-threads=1 > "$RUNNER_TEMP/tip-mounts.txt" 2>&1
for name in $mount_reds; do
  judge "at $tip" "$RUNNER_TEMP/tip-mounts.txt" "$name" passes
done
whole "linux_hidden_mounts --ignored at $tip" "$RUNNER_TEMP/tip-mounts.txt"
exit "$failed"
