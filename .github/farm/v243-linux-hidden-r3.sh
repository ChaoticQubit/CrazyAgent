set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
tip=6af8dcb26
fix=d7f4160d0
red=f144afb7b
base=dd648a01a
master_row="engine::owner::transport::master_host_launch::tests::master_sandbox_hides_a_listed_folder_from_a_real_child_on_linux"
lib_rows="sandbox::linux::tests::a_nest_takes_exact_read_only_empty_folder_pairs_before_its_working_folder $master_row"
lib_kept="sandbox::linux::tests::a_nest_takes_only_its_network_and_working_folder_before_the_command sandbox::linux::tests::bubblewrap_starts_the_agent_in_a_new_session engine::owner::transport::master_host_launch::tests::a_tools_off_master_reads_the_repository_but_cannot_move_its_refs"
helper=target/debug/crazyagent-landlock
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
git log --oneline -5
code_tip=$(git log -1 --format=%H HEAD -- . ':!.github')
if [ "$(git rev-parse --short=9 "$code_tip")" != "$tip" ] || [ "$(git rev-parse --short=9 "$code_tip~1")" != "$fix" ] || [ "$(git rev-parse --short=9 "$code_tip~2")" != "$red" ] || [ "$(git rev-parse --short=9 "$code_tip~3")" != "$base" ]; then
  row tip "$tip on $fix on $red on $base" "NOT: $(git log --format=%h --abbrev=9 -4 "$code_tip" | tr '\n' ' ')"
  exit 1
fi
bwrap=$(command -v bwrap)
"$bwrap" --version
if "$bwrap" --die-with-parent --unshare-user --unshare-pid --unshare-ipc --unshare-uts --dev-bind / / --proc /proc --unshare-net -- /bin/true; then
  row "this runner's bubblewrap nests (the master row runs nested)" yes yes
else
  row "this runner's bubblewrap nests (the master row runs nested)" yes "NOT: no"
  failed=1
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
build_helper() {
  local at=$1
  git checkout -q "$at"
  cargo build -p crazyagent --bin crazyagent-landlock --locked 2>&1 | tail -1
  git checkout -q "$code_tip"
  if [ -x "$helper" ]; then
    row "helper built at $at" built "built $(sha256sum "$helper" | cut -c1-12)"
  else
    row "helper built at $at" built "NOT: missing"
    failed=1
  fi
}
echo "== the lib test binary at $tip"
git checkout -q "$code_tip"
exe=$(cargo test -p crazyagent --lib --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build.txt" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))$/\1/p' | head -1)
tail -2 "$RUNNER_TEMP/build.txt"
if [ -z "$exe" ] || [ ! -x "$exe" ]; then
  row "lib test binary at $tip" built "NOT: none"
  exit 1
fi
exe=$(realpath "$exe")
row "lib test binary at $tip" built "$exe"
[ ! -e "$(dirname "$exe")/crazyagent-landlock" ] || { row "no helper beside the lib test binary" none "NOT: one"; failed=1; }
run_lib() {
  (cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" "$exe" "$@")
}
echo "== the master row at $tip with the helper built at $red (the r1 run's stale helper)"
build_helper "$red"
run_lib --exact "$master_row" > "$RUNNER_TEMP/stale.txt" 2>&1
judge "at $tip with the $red helper" "$RUNNER_TEMP/stale.txt" "$master_row" fails
if block "$master_row" "$RUNNER_TEMP/stale.txt" | grep -qF -- "--nest takes only --unshare-net and an absolute --chdir"; then
  row "the stale helper refuses the hidden folder cover (r1 cause)" "nest refusal in the child's stderr" yes
else
  row "the stale helper refuses the hidden folder cover (r1 cause)" "nest refusal in the child's stderr" "NOT: $(block "$master_row" "$RUNNER_TEMP/stale.txt" | grep -m1 -A3 'panicked at' | tail -3 | tr '\n' ' ' | cut -c1-300)"
  failed=1
fi
echo "== the lib rows at $tip with the helper built at $tip"
build_helper "$code_tip"
run_lib sandbox:: master_host_launch > "$RUNNER_TEMP/unit.txt" 2>&1
grep -E '^test result' "$RUNNER_TEMP/unit.txt" || true
for name in $lib_rows $lib_kept; do
  judge "at $tip" "$RUNNER_TEMP/unit.txt" "$name" passes
done
reds=$(grep -E '^test .* FAILED$' "$RUNNER_TEMP/unit.txt" | awk '{print $2}' | tr '\n' ' ')
for name in $reds; do
  echo "-- $name"
  block "$name" "$RUNNER_TEMP/unit.txt" | grep -A4 'panicked at' | head -6
done
row "lib sandbox:: master_host_launch at $tip" "record" "$(grep -E '^test result' "$RUNNER_TEMP/unit.txt" | tail -1)"
if [ -z "$reds" ]; then
  row "lib sandbox:: master_host_launch reds at $tip" none none
else
  row "lib sandbox:: master_host_launch reds at $tip" none "NOT: $reds"
  failed=1
fi
exit "$failed"
