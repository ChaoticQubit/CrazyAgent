#!/bin/bash
set -uo pipefail
os="${1:?usage: v243-paste-chunks.sh macos|linux}"
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
log="${RUNNER_TEMP:-/tmp}/v243-paste-chunks-row.log"
split=terminal::tmux::tests::typed_bytes_go_as_send_keys_commands_of_at_most_256_bytes_in_order
whole=terminal::tmux::tests::a_full_input_request_reaches_the_pane_whole_and_in_order_across_its_commands
patch=.github/farm/v243-paste-chunks-red.patch
rounds=3
failed=0

row() {
  printf '| %s | %s | %s | %s |\n' "$1" "$2" "$3" "$4" >> "$summary"
}

check() {
  local stage="$1" want="$2" test="$3" got
  cargo test -p crazyagent --lib --locked -- --exact "$test" 2>&1 | tee "$log"
  if grep -q "test result: ok. 1 passed" "$log"; then
    got=pass
  elif grep -q "test result: FAILED. 0 passed; 1 failed" "$log"; then
    got=fail
  else
    got="ran no row"
  fi
  row "$os $stage" "${test##*::}" "$want" "$got"
  [ "$got" = "$want" ] || failed=1
}

build() {
  cargo test -p crazyagent --lib --locked --no-run || {
    row "$os $1" "lib tests" compile "DID NOT COMPILE"
    exit 1
  }
}

printf '| stage | row | expected | result |\n| --- | --- | --- | --- |\n' >> "$summary"
case "$os" in
  macos | linux) ;;
  *)
    echo "unknown os: $os"
    exit 2
    ;;
esac
build tip
for round in $(seq 1 "$rounds"); do
  check "tip round $round" pass "$split"
  if [ "$os" = macos ]; then check "tip round $round" pass "$whole"; fi
done
if [ "$os" = macos ]; then
  cargo test -p crazyagent --lib --locked -- terminal::tmux::tests:: 2>&1 | tee "$log"
  result=$(grep -E "^test result:" "$log" | tail -1)
  row "$os tip" "terminal::tmux::tests (module, reported)" "all pass" "${result:-no result}"
fi
git apply "$patch" || {
  row "$os red" "$patch" applies "DID NOT APPLY"
  exit 1
}
build red
check red fail "$split"
if [ "$os" = macos ]; then check red pass "$whole"; fi
git apply -R "$patch" || { row "$os red" "patch reverts" reverts FAILS; failed=1; }
git diff --quiet || { row "$os" "tree" clean DIRTY; failed=1; }
exit "$failed"
