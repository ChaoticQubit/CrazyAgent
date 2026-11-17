set -euo pipefail
archive=${1:?usage: farm-rust-shard.sh <archive> <part> <parts>}
part=${2:?usage: farm-rust-shard.sh <archive> <part> <parts>}
parts=${3:?usage: farm-rust-shard.sh <archive> <part> <parts>}
if ! [[ "$part" =~ ^[1-9][0-9]*$ && "$parts" =~ ^[1-9][0-9]*$ ]] || (( part > parts )); then
  echo "::error::shard $part/$parts is no k/N with 1 <= k <= N"
  exit 2
fi
[ -f "$archive" ] || { echo "::error::no test archive at $archive; the build job for this platform failed"; exit 1; }
workspace=${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is not set}
[ "${NEXTEST_PROFILE:-}" = farm ] || { echo "::error::NEXTEST_PROFILE must be farm, the profile that writes target/nextest/farm/junit.xml"; exit 2; }
exe=""
[ "${RUNNER_OS:-}" != Windows ] || exe=.exe
no_tests=warn
if [ -n "${FARM_RERUN_FILTER:-}" ]; then
  filter=$FARM_RERUN_FILTER
  no_tests=fail
else
  filter=$(node scripts/ci/nextest-filter.mjs)
fi
repeats=${FARM_RERUN_REPEATS:-1}
[[ "$repeats" =~ ^[1-9]$ ]] || { echo "::error::FARM_RERUN_REPEATS must be 1 to 9, not $repeats"; exit 2; }
echo "filter: $filter"
echo "repeats: $repeats"
export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$workspace/target/debug/crazyagent$exe"
run=(cargo nextest run --archive-file "$archive" --workspace-remap "$workspace" --extract-to "$workspace" --extract-overwrite --partition "slice:$part/$parts" --no-tests="$no_tests" -E "$filter")
junit=${FARM_JUNIT:-target/nextest/farm/junit.xml}
status=0
for round in $(seq 1 "$repeats"); do
  echo "== farm round $round of $repeats"
  if [ "${RUNNER_OS:-}" = Linux ]; then
    dbus-run-session -- bash -euo pipefail -c 'printf "%s\n" ci-keyring | gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null; exec "$@"' farm "${run[@]}" || status=1
  else
    "${run[@]}" || status=1
  fi
  if [ -f "$junit" ]; then cp "$junit" "${junit%.xml}-round-$round.xml"; fi
done
exit "$status"
