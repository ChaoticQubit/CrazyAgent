#!/bin/bash
set -uo pipefail
label=$1
shift
out="$RUNNER_TEMP/$label"
mkdir -p "$out"
patch="$RUNNER_TEMP/plugin-probe.patch"
row=engine::owner::transport::barrier_tests::a_revocation_waits_for_a_plugin_change_in_flight_and_refuses_the_next
suites="storage:: engine::owner::native_auth:: engine::owner::transport::barrier_tests:: engine::owner::transport::usage_tests:: engine::owner::transport::recovery_tests:: engine::owner::transport::native_auth engine::owner::transport::master_runtime:: engine::owner::transport::accounts engine::bootstrap engine::native_auth_host::"
keyring() {
  dbus-run-session -- bash -uo pipefail -c 'printf "%s\n" ci-keyring | gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null; exec "$@"' farm "$@"
}
bad=0
for spec in "$@"; do
  commit=${spec%%:*}
  rest=${spec#*:}
  alone=${rest%%:*}
  runs=${rest#*:}
  git checkout -q --force --detach "$commit" || { echo "::error::cannot check out $commit"; exit 1; }
  git apply "$patch" || { echo "::error::the probe patch does not apply at $commit"; exit 1; }
  echo "== probe at $(git rev-parse --short=9 HEAD), patched"
  cargo build -p crazyagent --bin crazyagent-landlock --locked > "$out/helper-$commit.txt" 2>&1 || { tail -20 "$out/helper-$commit.txt"; echo "::error::landlock helper build failed at $commit"; exit 1; }
  mkdir -p target/debug/deps
  cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock
  cargo build -p crazyagent --bin crazyagent --locked > "$out/custodian-$commit.txt" 2>&1 || { tail -20 "$out/custodian-$commit.txt"; echo "::error::custodian build failed at $commit"; exit 1; }
  export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$PWD/target/debug/crazyagent"
  cargo test -p crazyagent --lib --locked --no-run > "$out/build-$commit.txt" 2>&1 || { grep -E '^(error|warning)' -A8 "$out/build-$commit.txt" | head -80; echo "::error::lib test build failed at $commit"; exit 1; }
  export PLUGINPROBE_OUT="$out/probe-$commit.txt"
  : > "$PLUGINPROBE_OUT"
  for i in $(seq 1 "$alone"); do
    keyring cargo test -p crazyagent --lib --locked -- --exact "$row" > "$out/alone-$commit-$i.txt" 2>&1
    if grep -qE "^test $row \.\.\. ok$" "$out/alone-$commit-$i.txt"; then echo "ALONE $commit try $i: ok"; elif grep -qE "^test $row \.\.\. FAILED$" "$out/alone-$commit-$i.txt"; then echo "ALONE $commit try $i: FAILED"; else echo "::error::the row did not run alone at $commit (try $i)"; tail -20 "$out/alone-$commit-$i.txt"; bad=1; fi
    echo "  $(tail -1 "$PLUGINPROBE_OUT")"
  done
  for i in $(seq 1 "$runs"); do
    keyring cargo test -p crazyagent --lib --locked -- $suites --skip native_auth_registers_the_installed_claude_code_executable > "$out/suites-$commit-$i.txt" 2>&1
    state=$(grep -oE "^test $row \.\.\. (ok|FAILED)$" "$out/suites-$commit-$i.txt" | awk '{print $NF}')
    others=$(grep -E '^test .* FAILED$' "$out/suites-$commit-$i.txt" | grep -vF "$row" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | tr '\n' ' ')
    echo "SUITE $commit run $i: row ${state:-missing}; $(grep -E '^test result' "$out/suites-$commit-$i.txt" | tail -1); other reds: ${others:-none}"
    echo "  $(tail -1 "$PLUGINPROBE_OUT")"
  done
  echo "PROBES $commit:"
  cat "$PLUGINPROBE_OUT"
  git checkout -q --force -- .
done
exit "$bad"
