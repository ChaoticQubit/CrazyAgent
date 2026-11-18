set +e -uo pipefail
label=${1:?usage: v244-batch.sh <label>}
out="$RUNNER_TEMP/v244-batch-$label.txt"
table="$RUNNER_TEMP/v244-batch-$label.md"
printf '| %s step | exit |\n| --- | --- |\n' "$label" > "$table"
: > "$out"
rc_of=()
step() {
  local name=$1
  shift
  "$@" 2>&1 | tee -a "$out"
  local rc=${PIPESTATUS[0]}
  echo "== $name rc=$rc" | tee -a "$out"
  printf '| %s | %s |\n' "$name" "$rc" >> "$table"
  rc_of+=("$rc")
}
[ "$RUNNER_OS" = Windows ] || { echo "::error::the bisect is Windows only, not $RUNNER_OS"; exit 1; }
cargo=(cargo +"$RUST_TOOLCHAIN")
rows=(
  engine::owner::transport::plugins::tests::a_check_container_with_no_stored_server_is_released_at_engine_start
  engine::owner::transport::plugins::tests::a_check_profile_that_cannot_be_released_stays_for_the_next_start
)
for sha in da95470d4 933efa14d; do
  git checkout -q --detach "$sha" || { echo "::error::cannot check out $sha"; exit 1; }
  git log --oneline -1 | tee -a "$out"
  step "$sha clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
  step "$sha test --lib --no-run" "${cargo[@]}" test -p crazyagent --lib --locked --no-run
  step "$sha two file_trash rows" "${cargo[@]}" test -p crazyagent --lib --locked --no-fail-fast -- --exact "${rows[@]}"
done
got="${rc_of[*]}"
pass() { [ "$1" = 0 ]; }
matched=no
if pass "${rc_of[0]}" && pass "${rc_of[1]}" && pass "${rc_of[2]}" && ! pass "${rc_of[3]}" && pass "${rc_of[4]}" && ! pass "${rc_of[5]}"; then matched=yes; fi
{
  cat "$table"
  echo
  echo "Exits: $got. Expected: green at da95470d4 (clippy, build, rows) and, at 933efa14d, a clippy red, a green build and red rows. Matched: $matched"
} >> "$GITHUB_STEP_SUMMARY"
echo "bisect matched=$matched exits=$got" | tee -a "$out"
[ "$matched" = yes ]
