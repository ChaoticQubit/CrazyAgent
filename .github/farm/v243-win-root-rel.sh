#!/usr/bin/env bash
set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
red=684ccb4a9388da1a715bb960d71f2c2c808f9e3b
fix=72da0514aefbbf20448b1a7d86e3e163a59b05db
code=spawn_relative_program_refused
red_rows=(
  spawn_gate::tests::a_root_relative_program_is_refused_where_another_user_could_plant_it_and_its_full_path_runs
  spawn_gate::tests::a_drive_relative_or_folder_relative_program_is_refused_and_a_bare_name_is_not
  worktrees::windows_tests::the_engine_git_command_is_refused_by_the_spawn_gate_on_windows
)
fix_rows=(
  "${red_rows[@]}"
  tools::windows_tests::a_program_is_fixed_only_when_absolute_or_a_bare_file_name
  coord::lane::windows_tests::the_windows_lane_still_runs_a_program_named_relative_to_its_folder
  spawn_gate::tests::entering_the_gate_inside_the_gate_panics_instead_of_deadlocking
)
failed=0
note() { printf '%s\n' "$1" >> "$summary"; echo "$1"; }
at() { git -c advice.detachedHead=false checkout -q "$1"; }
build_lib() {
  cargo test -p crazyagent --locked --lib --no-run > "$RUNNER_TEMP/build-$1.txt" 2>&1
  local rc=$?
  tail -n 5 "$RUNNER_TEMP/build-$1.txt"
  if [ "$rc" -ne 0 ]; then
    grep -E '^(error|warning)' -A 6 "$RUNNER_TEMP/build-$1.txt" | head -n 80
    note "| lib test build at $1 | builds | FAILED, exit $rc |"
    failed=1
    return 1
  fi
}
row() {
  local at="$1" name="$2" want="$3"
  cargo test -p crazyagent --locked --lib -- --exact "$name" > "$RUNNER_TEMP/row.txt" 2>&1
  cat "$RUNNER_TEMP/row.txt"
  local got=missing
  grep -qE "^test $name( - should panic)? \.\.\. ok$" "$RUNNER_TEMP/row.txt" && got=ok
  grep -qE "^test $name( - should panic)? \.\.\. FAILED$" "$RUNNER_TEMP/row.txt" && got=FAILED
  if [ "$got" = "$want" ]; then
    note "| ${name##*::} at $at | $want | $got |"
  else
    note "| ${name##*::} at $at | $want | **$got** |"
    failed=1
  fi
}
case "${1:-}" in
  rows)
    note "| row | expected | result |"
    note "| --- | --- | --- |"
    drive=$(pwd -W | cut -c1-2)
    echo "P9: current drive $drive"
    MSYS_NO_PATHCONV=1 icacls 'C:\'
    MSYS_NO_PATHCONV=1 icacls "$drive\\"
    at "$red"
    if build_lib red; then
      for name in "${red_rows[@]}"; do row red "$name" FAILED; done
    fi
    at "$fix"
    for profile in dev test; do
      cargo clippy -p crazyagent --locked --lib --profile "$profile" -- -D warnings > "$RUNNER_TEMP/clippy-$profile.txt" 2>&1
      rc=$?
      grep -E '^(warning|error)' -A 8 "$RUNNER_TEMP/clippy-$profile.txt" | head -n 120
      tail -n 3 "$RUNNER_TEMP/clippy-$profile.txt"
      if [ "$rc" -eq 0 ]; then note "| clippy --lib --profile $profile at fix | clean | clean |"; else note "| clippy --lib --profile $profile at fix | clean | **exit $rc** |"; failed=1; fi
    done
    if build_lib fix; then
      for name in "${fix_rows[@]}"; do row fix "$name" ok; done
    fi
    ;;
  suite)
    at "$fix"
    cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/bins.txt" 2>&1
    rc=$?
    tail -n 5 "$RUNNER_TEMP/bins.txt"
    if [ "$rc" -ne 0 ]; then note "| engine binaries at fix | build | FAILED, exit $rc |"; echo "FARM failed=1"; exit 1; fi
    mkdir -p target/debug/deps
    cp target/debug/crazyagent-landlock.exe target/debug/deps/crazyagent-landlock.exe
    cargo build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked > "$RUNNER_TEMP/boundary.txt" 2>&1
    rc=$?
    tail -n 5 "$RUNNER_TEMP/boundary.txt"
    if [ "$rc" -ne 0 ]; then note "| crazyagent-win-boundary at fix | build | FAILED, exit $rc |"; echo "FARM failed=1"; exit 1; fi
    cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe
    build_lib fix || { echo "FARM failed=1"; exit 1; }
    cargo test -p crazyagent --locked --lib > "$RUNNER_TEMP/suite.txt" 2>&1
    rc=$?
    grep -E '^test result:' "$RUNNER_TEMP/suite.txt"
    echo "=== failed rows"
    grep -E '^test .* \.\.\. FAILED$' "$RUNNER_TEMP/suite.txt" | tee "$RUNNER_TEMP/failed.txt"
    echo "=== lines naming $code"
    grep -nF "$code" "$RUNNER_TEMP/suite.txt" | head -n 60
    count=$(grep -cE '^test .* \.\.\. FAILED$' "$RUNNER_TEMP/suite.txt")
    named=$(grep -cF "$code" "$RUNNER_TEMP/suite.txt")
    note "| full --lib at fix | exit | $rc, $count failed rows, $named lines naming $code |"
    [ "$rc" -eq 0 ] || failed=1
    echo "=== failures section"
    sed -n '/^failures:$/,$p' "$RUNNER_TEMP/suite.txt" | head -n 400
    ;;
  *)
    echo "usage: rows | suite"; exit 2 ;;
esac
echo "FARM failed=$failed"
exit "$failed"
