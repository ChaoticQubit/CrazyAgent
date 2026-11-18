set +e -uo pipefail
label=${1:?usage: v244-batch.sh <label>}
out="$RUNNER_TEMP/v244-batch-$label.txt"
summary="$RUNNER_TEMP/ab-summary.md"
: > "$out"
tries=3
case "$RUNNER_OS" in
  Windows)
    rows=(
      harnesses::probe_tests::a_cached_catalog_is_served_while_its_re_probe_runs_and_a_check_probes_now
      harnesses::probe_tests::a_cached_catalog_whose_harness_file_is_gone_is_probed_before_it_answers
      research::accounts::accounts_tests::research_runs_on_endpoint_accounts_with_or_without_a_key_by_wire_format
      service::tests::link_cli_adds_a_folder_named_like_powershell_code_to_the_user_path_once
      ledger::tests::pending_actions_are_bounded_and_finished_ones_are_pruned_to_the_kept_count
    )
    exe=.exe
    ;;
  Linux)
    rows=(
      engine::owner::transport::owner_deadline_tests::a_skill_import_slower_than_the_owner_deadline_answers_with_the_skill
      engine::owner::transport::secret_storage_wait_tests::a_checked_server_removed_while_storage_is_busy_leaves_no_check_folder
      engine::owner::transport::secret_storage_wait_tests::a_server_added_with_secrets_while_storage_is_busy_is_withdrawn_with_every_one_of_them
      engine::owner::transport::secret_storage_wait_tests::a_server_removed_while_storage_is_busy_leaves_none_of_its_secrets_on_disk
      engine::owner::transport::secret_storage_wait_tests::a_server_secret_removed_while_storage_is_busy_leaves_no_file_on_disk
      engine::owner::transport::secret_storage_wait_tests::a_server_secret_replaced_while_storage_is_busy_is_withdrawn_and_keeps_only_the_old_one
      engine::owner::transport::secret_storage_wait_tests::an_account_removed_while_storage_is_busy_leaves_no_key_on_disk
    )
    exe=""
    ;;
  *)
    echo "::error::no A/B recipe for RUNNER_OS=$RUNNER_OS"
    exit 1
    ;;
esac
keyring() {
  dbus-run-session -- bash -euo pipefail -c 'printf "%s\n" ci-keyring | gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null; exec "$@"' farm "$@"
}
terms=$(printf 'test(=%s)\n' "${rows[@]}" | paste -sd'|' - | sed 's/|/ | /g')
filter="package(=crazyagent) & kind(lib) & ($terms)"
export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$GITHUB_WORKSPACE/target/debug/crazyagent$exe"
printf '| %s row | failed at chore/v243 ecef0204e of %s | failed at the merge c503b8d0b of %s |\n| --- | --- | --- |\n' "$label" "$tries" "$tries" > "$summary"
for sha in ecef0204e c503b8d0b; do
  git checkout -q --detach "$sha" || { echo "::error::cannot check out $sha"; exit 1; }
  git log --oneline -1 | tee -a "$out"
  features=()
  if grep -Eq '^owner-trace[[:space:]]*=' crates/crazyagent/Cargo.toml; then features=(--features crazyagent/owner-trace); fi
  if [ "$RUNNER_OS" = Windows ]; then
    cargo build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked 2>&1 | tail -2 | tee -a "$out"
    mkdir -p target/debug/deps && cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe
    cargo build -p crazyagent --bin crazyagent "${features[@]}" --locked 2>&1 | tail -2 | tee -a "$out"
  else
    cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock "${features[@]}" --locked 2>&1 | tail -2 | tee -a "$out"
    mkdir -p target/debug/deps && cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock
  fi
  cargo nextest run -p crazyagent --lib "${features[@]}" --locked --profile farm --no-run 2>&1 | tail -3 | tee -a "$out"
  : > "$RUNNER_TEMP/fails-$sha.txt"
  for try in $(seq 1 "$tries"); do
    if [ "$RUNNER_OS" = Linux ]; then
      keyring cargo nextest run -p crazyagent --lib "${features[@]}" --locked --profile farm --no-fail-fast -E "$filter" > "$RUNNER_TEMP/try.txt" 2>&1
    else
      cargo nextest run -p crazyagent --lib "${features[@]}" --locked --profile farm --no-fail-fast -E "$filter" > "$RUNNER_TEMP/try.txt" 2>&1
    fi
    tr -d '\r' < "$RUNNER_TEMP/try.txt" | tee -a "$out" | grep -E '^[[:space:]]*(FAIL|TIMEOUT|SIGSEGV|SIGABRT|SIGKILL|SIGTERM|ABORT) \[' | awk '{print $NF}' | sort -u >> "$RUNNER_TEMP/fails-$sha.txt"
    echo "== $sha try $try done" | tee -a "$out"
  done
done
new=0
for row in "${rows[@]}"; do
  a=$(grep -cxF "$row" "$RUNNER_TEMP/fails-ecef0204e.txt")
  b=$(grep -cxF "$row" "$RUNNER_TEMP/fails-c503b8d0b.txt")
  printf '| %s | %s | %s |\n' "$row" "$a" "$b" >> "$summary"
  echo "== ${row} before=$a/$tries batch=$b/$tries" | tee -a "$out"
  if [ "$a" = 0 ] && [ "$b" != 0 ]; then new=1; fi
done
cat "$summary" >> "$GITHUB_STEP_SUMMARY"
[ "$new" = 0 ]
