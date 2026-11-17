#!/bin/bash
set -uo pipefail
os="$1"
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0

row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"
}

green() {
  local name="$1"
  shift
  if "$@"; then
    row "green: $name" pass pass
  else
    row "green: $name" pass FAIL
    failed=1
  fi
}

red() {
  local patch=".github/farm/$1.patch"
  shift
  if ! git apply "$patch"; then
    row "red: $patch" applies "DID NOT APPLY"
    failed=1
    return
  fi
  if ! cargo test -p crazyagent --lib --locked --no-run; then
    row "red: $patch" compiles "DID NOT COMPILE"
    failed=1
  else
    local test
    for test in "$@"; do
      if cargo test -p crazyagent --lib --locked -- --exact "$test"; then
        row "red: $patch $test" fails "PASSED"
        failed=1
      else
        row "red: $patch $test" fails fails
      fi
    done
  fi
  git apply -R "$patch"
}

printf '| run | expected | result |\n| --- | --- | --- |\n' >> "$summary"
case "$os" in
  macos)
    green "pure rows" cargo test -p crazyagent --lib --locked -- \
      launch_context::tests::a_launch_that_left \
      launch_context::tests::batch_files_tests:: \
      plugins::tests:: \
      tools::batch::
    green "desktop copy" node --test apps/desktop/test/settings-plugins.test.mjs
    red mcp-npx-red-macos \
      launch_context::tests::batch_files_tests::a_launch_that_left_batch_file_servers_out_names_them_in_its_tools_section \
      plugins::tests::a_check_of_a_batch_command_says_what_to_point_it_at \
      tools::batch::tests::a_program_name_is_read_as_windows_reads_it
    git apply .github/farm/mcp-npx-red-macos.patch
    if node --test apps/desktop/test/settings-plugins.test.mjs; then
      row "red: desktop copy" fails "PASSED"
      failed=1
    else
      row "red: desktop copy" fails fails
    fi
    git apply -R .github/farm/mcp-npx-red-macos.patch
    ;;
  windows)
    green "windows rows" cargo test -p crazyagent --lib --locked -- --exact \
      plugins::windows_tests::npm_s_npx_cmd_runs_as_node_and_every_other_batch_command_is_left_out \
      storage::plugins::windows_tests::a_batch_file_server_is_left_out_of_a_launch_by_name \
      engine::owner::transport::plugins::tests::a_windows_command_server_check_runs_in_its_boundary_or_says_why_not \
      tools::windows_tests::npx_off_the_engine_s_path_runs_with_the_node_beside_it
    red mcp-npx-red-windows-sec \
      plugins::windows_tests::npm_s_npx_cmd_runs_as_node_and_every_other_batch_command_is_left_out \
      storage::plugins::windows_tests::a_batch_file_server_is_left_out_of_a_launch_by_name
    red mcp-npx-red-windows-code \
      engine::owner::transport::plugins::tests::a_windows_command_server_check_runs_in_its_boundary_or_says_why_not \
      tools::windows_tests::npx_off_the_engine_s_path_runs_with_the_node_beside_it \
      plugins::windows_tests::npm_s_npx_cmd_runs_as_node_and_every_other_batch_command_is_left_out
    ;;
  *)
    echo "unknown os $os"
    exit 2
    ;;
esac
git diff --quiet || { row "tree" clean "DIRTY"; failed=1; }
exit "$failed"
