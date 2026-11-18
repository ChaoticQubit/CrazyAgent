set -uo pipefail
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
failed=0
tip=fdb9b311b
base=44b11b54e
host=packages.microsoft.com
work="$RUNNER_TEMP/apt"
mkdir -p "$work/kept"
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3" >> "$summary"; }
printf '| check | expected | result |\n| --- | --- | --- |\n' >> "$summary"
uname -r
git log --oneline -3
code_tip=$(git log -1 --format=%H HEAD -- . ':!.github/farm' ':!.github/workflows/tests.yml')
if [ "$(git rev-parse --short=9 "$code_tip")" != "$tip" ] || [ "$(git rev-parse --short=9 "$code_tip~1")" != "$base" ]; then
  row tip "$tip on $base" "NOT: $(git log --format=%h --abbrev=9 -2 "$code_tip" | tr '\n' ' ')"
  exit 1
fi
command -v yq >/dev/null || { row yq present "NOT: missing"; exit 1; }
yq --version
for sha in "$base" "$tip"; do
  git show "$sha:scripts/ci/linux-packages.sh" > "$work/script-$sha.sh"
  git show "$sha:.github/workflows/ci.yml" > "$work/ci-$sha.yml"
  git show "$sha:.github/workflows/release.yml" > "$work/release-$sha.yml"
  yq '.jobs.linux.steps[] | select(.name == "Install Linux build packages") | .run' "$work/ci-$sha.yml" > "$work/ci-$sha.sh"
  yq '.jobs.build.steps[] | select(.name == "Install Linux build packages") | .run' "$work/release-$sha.yml" > "$work/release-$sha.sh"
done
for file in "$work"/*.sh; do
  if [ "$(head -1 "$file")" = "set -euo pipefail" ] && bash -n "$file"; then
    echo "$(basename "$file"): $(wc -l < "$file") lines, $(grep -c "$host" "$file") naming $host"
  else
    row "extract $(basename "$file")" "a bash block" "NOT: empty or not bash"
    exit 1
  fi
done
named() { grep -l "$host" /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources 2>/dev/null | tr '\n' ' '; }
sources=$(named)
if [ -z "$sources" ]; then
  row "runner image source" "a file naming $host" "NOT: none, so this job proves nothing"
  exit 1
fi
for file in $sources; do
  echo "== $file"
  cat "$file"
  cp -p "$file" "$work/kept/"
done
row "runner image source" "a file naming $host" "$sources"
restore() { sudo cp -p "$work"/kept/* /etc/apt/sources.list.d/; }

echo "== the packages each install line pulls, with $host among the sources"
sudo apt-get update > "$work/update.txt" 2>&1
grep -E "$host|^(Err|E):" "$work/update.txt" | head -8
: > "$work/offered.txt"
for list in /var/lib/apt/lists/"${host}"_*_Packages*; do
  [ -f "$list" ] && /usr/lib/apt/apt-helper cat-file "$list" | sed -n 's/^Package: //p' >> "$work/offered.txt"
done
sort -u -o "$work/offered.txt" "$work/offered.txt"
offered=$(wc -l < "$work/offered.txt")
echo "$host offers $offered package names"
script_list=$(sed -n 's/^packages=(\(.*\))$/\1/p' "$work/script-$tip.sh")
ci_list=$(sed -n 's/^packages=(\(.*\))$/\1/p' "$work/ci-$tip.sh")
release_list=$(grep -- '--no-install-recommends' "$work/release-$tip.sh" | sed 's/.*--no-install-recommends //')
for name in script ci release; do
  var="${name}_list"
  words=${!var}
  if [ -z "$words" ]; then
    row "$name install line" "package names" "NOT: none read"
    failed=1
    continue
  fi
  echo "== $name asks for: $words"
  apt-get install --simulate --no-install-recommends $words > "$work/sim-$name.txt" 2>&1
  code=$?
  grep '^Inst ' "$work/sim-$name.txt" > "$work/inst-$name.txt" || true
  awk '{print $2}' "$work/inst-$name.txt" | sort -u > "$work/names-$name.txt"
  echo "-- $name installs $(wc -l < "$work/names-$name.txt") packages (simulate exit $code):"
  sed -E 's/^Inst ([^ ]+) (\[[^]]*\] )?\(([^ ]+) ([^ ]+) .*$/\1 \3 \4/' "$work/inst-$name.txt"
  echo "-- $name origins:"
  sed -E 's/^Inst [^ ]+ (\[[^]]*\] )?\([^ ]+ ([^ ]+) .*$/\2/' "$work/inst-$name.txt" | sort | uniq -c
  microsoft=$(grep -ci microsoft "$work/inst-$name.txt")
  shared=$(comm -12 "$work/names-$name.txt" "$work/offered.txt" | tr '\n' ' ')
  if [ "$code" -eq 0 ] && [ "$microsoft" -eq 0 ] && [ -z "$shared" ]; then
    row "$name install line" "no package from $host" "$(wc -l < "$work/names-$name.txt") packages, 0 with a Microsoft origin, 0 of $offered names $host offers"
  else
    row "$name install line" "no package from $host" "NOT: simulate exit $code, $microsoft Microsoft origins, shared: ${shared:-none}"
    failed=1
  fi
done
if [ "$offered" -eq 0 ]; then
  row "$host index" "fetched, so the check above means something" "NOT: no index fetched; the origin check is empty"
fi

echo "== $host unreachable"
echo "127.0.0.1 $host" | sudo tee -a /etc/hosts >/dev/null
getent hosts "$host"
run() {
  local label=$1 file=$2 environment=$3 want=$4
  restore
  SECONDS=0
  RUNNER_ENVIRONMENT=$environment bash "$file" > "$work/run-$label.txt" 2>&1
  local code=$? took=$SECONDS
  local left notices
  left=$(named)
  notices=$(grep -c '::notice::removed the apt source' "$work/run-$label.txt")
  grep -E "$host|::(notice|warning|error)::" "$work/run-$label.txt" | head -6
  echo "== $label: exit $code in ${took}s, sources naming $host: ${left:-none}, notices: $notices"
  local got="exit $code, sources naming the host: ${left:-none}, notices $notices"
  case $want in
    red) [ "$code" -ne 0 ] && [ -n "$left" ] && [ "$notices" -eq 0 ] ;;
    kept) [ "$code" -ne 0 ] && [ -n "$left" ] && [ "$notices" -eq 0 ] && grep -q 'fix the runner' "$work/run-$label.txt" ;;
    green) [ "$code" -eq 0 ] && [ -z "$left" ] && [ "$notices" -ge 1 ] ;;
  esac
  if [ $? -eq 0 ]; then
    row "$label" "$want" "$got"
  else
    row "$label" "$want" "NOT: $got"
    tail -15 "$work/run-$label.txt"
    failed=1
  fi
}
run "ci.yml at $base" "$work/ci-$base.sh" github-hosted red
run "ci.yml at $tip, self-hosted" "$work/ci-$tip.sh" self-hosted kept
run "ci.yml at $tip" "$work/ci-$tip.sh" github-hosted green
run "release.yml at $base" "$work/release-$base.sh" github-hosted red
run "release.yml at $tip, self-hosted" "$work/release-$tip.sh" self-hosted kept
run "release.yml at $tip" "$work/release-$tip.sh" github-hosted green
run "linux-packages.sh at $base" "$work/script-$base.sh" github-hosted red
run "linux-packages.sh at $tip" "$work/script-$tip.sh" github-hosted green
exit "$failed"
