set +e -uo pipefail
label=${1:?usage: v244-batch.sh <label>}
cp .github/farm/linux50.txt "$RUNNER_TEMP/linux50.txt"
out="$RUNNER_TEMP/v244-batch-$label.txt"
table="$RUNNER_TEMP/v244-batch-$label.md"
: > "$out"
printf '| %s step | exit |\n| --- | --- |\n' "$label" > "$table"
step() {
  local name=$1
  shift
  "$@" 2>&1 | tee -a "$out"
  local rc=${PIPESTATUS[0]}
  echo "== $name rc=$rc" | tee -a "$out"
  printf '| %s | %s |\n' "$name" "$rc" >> "$table"
}
[ "$RUNNER_OS" = Linux ] || { echo "::error::Linux only, not $RUNNER_OS"; exit 1; }
terms=$(sed 's/.*/test(=&)/' "$RUNNER_TEMP/linux50.txt" | paste -sd'|' - | sed 's/|/ | /g')
filter="package(=crazyagent) & kind(lib) & ($terms)"
echo "filter rows: $(wc -l < "$RUNNER_TEMP/linux50.txt")" | tee -a "$out"
keyring() {
  dbus-run-session -- bash -euo pipefail -c 'printf "%s\n" ci-keyring | gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null; exec "$@"' farm "$@"
}
for sha in 32f588970 4efef630d; do
  git checkout -q --detach "$sha" || { echo "::error::cannot check out $sha"; exit 1; }
  git log --oneline -1 | tee -a "$out"
  features=()
  if grep -Eq '^owner-trace[[:space:]]*=' crates/crazyagent/Cargo.toml; then features=(--features crazyagent/owner-trace); fi
  step "$sha build crazyagent and landlock bins" cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock "${features[@]}" --locked
  mkdir -p target/debug/deps && cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock
  export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$GITHUB_WORKSPACE/target/debug/crazyagent"
  step "$sha nextest build" cargo nextest run -p crazyagent --lib "${features[@]}" --locked --profile farm --no-run
  keyring cargo nextest run -p crazyagent --lib "${features[@]}" --locked --profile farm --no-fail-fast -E "$filter" 2>&1 | tee "$RUNNER_TEMP/rows-$sha.txt" | tee -a "$out"
  rc=${PIPESTATUS[0]}
  grep -E '^[[:space:]]*(FAIL|TIMEOUT|SIGSEGV|SIGABRT|SIGKILL|SIGTERM|ABORT|LEAK-FAIL) \[' "$RUNNER_TEMP/rows-$sha.txt" | awk '{print $NF}' | sort -u > "$RUNNER_TEMP/failed-$sha.txt"
  passed=$(grep -cE '^[[:space:]]*PASS \[' "$RUNNER_TEMP/rows-$sha.txt")
  echo "== $sha rows rc=$rc passed=$passed failed=$(wc -l < "$RUNNER_TEMP/failed-$sha.txt")" | tee -a "$out"
  printf '| %s the 50 rows | rc=%s, passed %s, failed %s |\n' "$sha" "$rc" "$passed" "$(wc -l < "$RUNNER_TEMP/failed-$sha.txt")" >> "$table"
done
new=$(comm -13 "$RUNNER_TEMP/failed-32f588970.txt" "$RUNNER_TEMP/failed-4efef630d.txt")
both=$(comm -12 "$RUNNER_TEMP/failed-32f588970.txt" "$RUNNER_TEMP/failed-4efef630d.txt")
fixed=$(comm -23 "$RUNNER_TEMP/failed-32f588970.txt" "$RUNNER_TEMP/failed-4efef630d.txt")
{
  cat "$table"
  echo
  echo "Fail only at 4efef630d (batch regressions): $(printf '%s' "$new" | grep -c .)"
  [ -z "$new" ] || printf -- '- %s\n' $new
  echo
  echo "Fail at both (before the batch): $(printf '%s' "$both" | grep -c .)"
  [ -z "$both" ] || printf -- '- %s\n' $both
  echo
  echo "Fail only at 32f588970: $(printf '%s' "$fixed" | grep -c .)"
  [ -z "$fixed" ] || printf -- '- %s\n' $fixed
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"
[ -z "$new" ]
