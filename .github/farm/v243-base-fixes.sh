#!/bin/bash
set -uo pipefail
os="${1:?usage: v243-base-fixes.sh macos|linux}"
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
log="${RUNNER_TEMP:-/tmp}/v243-base-fixes-row.log"
codes=engine::owner::transport::changes::tests::changes_codes_cover_every_refusal_of_the_landing_engine_and_protocol
patch=.github/farm/v243-base-fixes-red.patch
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
  if cargo test -p crazyagent --lib --locked --no-run; then
    row "$os $1" "lib tests" compile compiled
  else
    row "$os $1" "lib tests" compile "DID NOT COMPILE"
    exit 1
  fi
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
  check "tip round $round" pass "$codes"
done
git apply "$patch" || {
  row "$os base" "$patch" applies "DID NOT APPLY"
  exit 1
}
build "base row"
check "base row" fail "$codes"
git apply -R "$patch" || { row "$os base" "patch reverts" reverts FAILS; failed=1; }
git diff --quiet || { row "$os" "tree" clean DIRTY; failed=1; }
exit "$failed"
