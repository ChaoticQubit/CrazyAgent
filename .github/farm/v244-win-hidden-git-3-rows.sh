#!/usr/bin/env bash
set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
rows=(sandbox::hidden_paths:: project_files::
  tui::screens::project_tests::a_hidden_folder_refused
  tui::screens::project_tests::settings_offer_hidden_folders
  project_setup::engine_tests::hidden_folders_are_refused
  project_setup::engine_tests::stale_hidden_paths
  project_setup::engine_tests::hidden_paths_read_names)
if [ "$RUNNER_OS" = Windows ]; then
  exe=.exe
else
  exe=
fi
note() { printf '%s\n' "$1" | tee -a "$summary"; }
at() {
  git checkout -q -f --detach "$1" || { echo "checkout $1 failed"; exit 1; }
  echo "######## at $(git log --oneline -1)"
}
helpers() {
  cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/bins.txt" 2>&1 || { tail -n 20 "$RUNNER_TEMP/bins.txt"; return 1; }
  mkdir -p target/debug/deps
  cp "target/debug/crazyagent-landlock$exe" "target/debug/deps/crazyagent-landlock$exe"
  if [ "$RUNNER_OS" = Windows ]; then
    cargo build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked > "$RUNNER_TEMP/boundary.txt" 2>&1 || { tail -n 20 "$RUNNER_TEMP/boundary.txt"; return 1; }
    cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe
  fi
  cargo test -p crazyagent --locked --lib --no-run > "$RUNNER_TEMP/lib.txt" 2>&1 || { grep -E '^(error|warning)' -A 6 "$RUNNER_TEMP/lib.txt" | head -n 80; return 1; }
}
run() {
  local label=$1
  shift
  cargo test -p crazyagent --locked --lib -- "$@" > "$RUNNER_TEMP/$label.txt" 2>&1
  local rc=$?
  grep -E '^test .* \.\.\. |^test result:' "$RUNNER_TEMP/$label.txt"
  sed -n '/^failures:$/,$p' "$RUNNER_TEMP/$label.txt" | head -n 300
  grep -E '^test .* \.\.\. FAILED$' "$RUNNER_TEMP/$label.txt" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sort > "$RUNNER_TEMP/$label.failed"
  passed=$(grep -cE '^test .* \.\.\. ok$' "$RUNNER_TEMP/$label.txt")
  count=$(grep -c . "$RUNNER_TEMP/$label.failed")
  return $rc
}
mode=$1
sha=$2
shift 2
case "$mode" in
  build)
    at "$sha"
    helpers || { note "| build at $sha | FAILED |"; exit 1; }
    note "| build at $sha | ok |"
    ;;
  red)
    at "$sha"
    run "red-${sha:0:9}" "${rows[@]}"
    printf '%s\n' "$@" | sort > "$RUNNER_TEMP/want.failed"
    if diff -u "$RUNNER_TEMP/want.failed" "$RUNNER_TEMP/red-${sha:0:9}.failed"; then
      note "| red ${sha:0:9} | $count failed, exactly the $# expected on $RUNNER_OS; $passed ok |"
    else
      note "| red ${sha:0:9} | **failed set differs from the $# expected** ($count failed) |"
      exit 1
    fi
    ;;
  tip)
    at "$sha"
    run tip "${rows[@]}"
    rc=$?
    failed=0
    if [ "$rc" -eq 0 ] && [ "$count" -eq 0 ] && [ "$passed" -gt 0 ]; then
      note "| tip ${sha:0:9} rows | exit 0, $passed ok |"
    else
      note "| tip ${sha:0:9} rows | **exit $rc, $count failed** |"
      failed=1
    fi
    run broad hidden
    rc=$?
    note "| tip, every lib row naming hidden | exit $rc, $passed ok, $count failed |"
    cp "$RUNNER_TEMP/broad.failed" "$RUNNER_TEMP/broad.list"
    exit "$failed"
    ;;
  base)
    if [ ! -s "$RUNNER_TEMP/broad.list" ]; then
      note "| base | no row naming hidden is red at the tip |"
      exit 0
    fi
    at "$sha"
    helpers || { note "| build at base | FAILED |"; exit 1; }
    failed=0
    while read -r name; do
      cargo test -p crazyagent --locked --lib -- --exact "$name" --test-threads 1 > "$RUNNER_TEMP/again.txt" 2>&1
      if grep -qE "^test $name \.\.\. FAILED$" "$RUNNER_TEMP/again.txt"; then
        note "| $name | red at tip and at base ${sha:0:9}: not this branch |"
      else
        note "| $name | **red at tip only** |"
        failed=1
      fi
    done < "$RUNNER_TEMP/broad.list"
    exit "$failed"
    ;;
  *)
    echo "unknown mode $mode"
    exit 1
    ;;
esac
