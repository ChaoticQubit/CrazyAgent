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
rows=$(grep -E '^[a-z_]+(::[a-z0-9_]+)+$' .github/farm/baseline-47.txt | sed -E 's/^(.*)$/test(=\1)/' | paste -sd'|' -)
[ "$(grep -cE '^[a-z_]+(::[a-z0-9_]+)+$' .github/farm/baseline-47.txt)" = 47 ] || { echo "::error::the baseline list is not 47 library rows"; exit 2; }
filter="package(=crazyagent) & kind(lib) & ($rows)"
echo "filter: the 47 baseline rows"
export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$workspace/target/debug/crazyagent$exe"
run=(cargo nextest run --archive-file "$archive" --workspace-remap "$workspace" --extract-to "$workspace" --extract-overwrite --partition "slice:$part/$parts" --no-tests=warn -E "$filter")
junit=target/nextest/farm/junit.xml
bad=0
for attempt in 1 2 3; do
  rm -f "$junit"
  "${run[@]}" || bad=1
  [ -s "$junit" ] || { echo "::error::run $attempt wrote no JUnit report"; exit 1; }
  cp "$junit" "target/nextest/farm/junit-run-$attempt.xml"
done
python3 -I - target/nextest/farm <<'PY'
import pathlib, sys, xml.etree.ElementTree as ET
folder = pathlib.Path(sys.argv[1])
root = ET.Element("testsuites")
for attempt in (1, 2, 3):
    for suite in ET.parse(folder / f"junit-run-{attempt}.xml").getroot().iter("testsuite"):
        suite.set("name", f"{suite.get('name')} run {attempt}")
        root.append(suite)
ET.ElementTree(root).write(folder / "junit.xml")
for attempt in (1, 2, 3):
    (folder / f"junit-run-{attempt}.xml").unlink()
PY
exit "$bad"
