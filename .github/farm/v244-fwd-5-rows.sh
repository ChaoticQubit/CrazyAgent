set -uo pipefail
tip=238ab31c7
merge=85dd3f127
first=be1f74f78
second=84d2bf57a
os=${FARM_OS:?FARM_OS is linux, windows or macos}
exe=""
[ "$os" != windows ] || exe=.exe
out="$RUNNER_TEMP/fwd5"
mkdir -p "$out"
bad=0
short() { git rev-parse --short=9 "$1"; }
say() { printf '%s\n' "$*"; printf '%s\n\n' "$*" >> "${GITHUB_STEP_SUMMARY:-/dev/null}"; }
keyring() {
  if [ "$os" = linux ]; then
    dbus-run-session -- bash -euo pipefail -c 'printf "%s\n" ci-keyring | gnome-keyring-daemon --replace --daemonize --unlock --components=secrets >/dev/null; exec "$@"' farm "$@"
  else
    "$@"
  fi
}
case "$os" in
  linux)
    CORE="context_mode::tests:: config::tests:: engine::bootstrap::tests:: engine::owner::transport:: storage::native_auth_custody_tests:: storage::model_access_page_tests:: storage::native_auth_host_tests:: storage::projects_home_tests::"
    DESK="tests::"
    CLIPPY=1
    ;;
  windows)
    CORE="context_mode::windows_tests:: storage::windows_tests:: config::tests:: engine::owner::transport::plugins::tests::"
    DESK="tests::"
    CLIPPY=1
    ;;
  macos)
    CORE="engine::owner::transport::"
    DESK=""
    CLIPPY=0
    ;;
  *) echo "bad FARM_OS $os"; exit 2 ;;
esac
DRIVE=context_mode::windows_tests::a_drive_root_no_app_container_reads_refuses_the_install_by_its_own_code
code=$(git log -1 --format=%H HEAD -- . ':!.github')
parents=$(git log -1 --format=%p --abbrev=9 "$merge")
if [ "$(short "$code")" != "$tip" ] || [ "$(short "$tip~1")" != "$merge" ] || [ "$parents" != "$first $second" ]; then
  say "chain NOT: code $(short "$code") on $(short "$code~1"), merge parents $parents, want $tip on $merge = $first + $second"
  exit 1
fi
say "chain $tip on $merge = $first + $second yes; git $(git --version); runner ${ImageOS:-unknown} ${ImageVersion:-unknown}"
checkout() {
  git checkout -q -f --detach "$1" || return 1
  echo "tree $(short HEAD) $(git status --short | grep -v '^??' | tr '\n' ' ')"
}
bins() {
  local log="$out/bins-$1.log"
  cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock --locked > "$log" 2>&1 || { tail -20 "$log" | cut -c1-300; return 1; }
  mkdir -p target/debug/deps
  cp "target/debug/crazyagent-landlock$exe" "target/debug/deps/crazyagent-landlock$exe"
  if [ "$os" = windows ]; then
    cargo build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked >> "$log" 2>&1 || { tail -20 "$log" | cut -c1-300; return 1; }
    cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe
  fi
  export CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$PWD/target/debug/crazyagent$exe"
}
listed() {
  local pkg=$1 at=$2 log="$out/list-$1-$2.txt"
  keyring cargo test -p "$pkg" --lib --locked -- --list > "$log" 2> "$out/list-$1-$2.err" || { tail -30 "$out/list-$1-$2.err" | cut -c1-300; return 1; }
  echo "$pkg lib rows at $at: $(grep -c ': test$' "$log")"
}
clippy() {
  local at=$1 pkg kind log rc
  for pkg in crazyagent crazyagent-desktop; do
    for kind in --lib --tests; do
      log="$out/clippy-$pkg$kind-$at.log"
      cargo clippy -p "$pkg" "$kind" --locked -- -D warnings > "$log" 2>&1
      rc=$?
      say "clippy $pkg $kind at $at rc=$rc, $(grep -cE '^(warning|error)' "$log") warning/error lines"
      [ "$rc" -eq 0 ] || { grep -E -A6 '^(warning|error)' "$log" | head -40 | cut -c1-300; echo "$pkg $kind" >> "$out/clippy-red-$at"; }
    done
  done
}
run_rows() {
  local pkg=$1 at=$2 names=$3 tag=$4 log="$out/rows-$4-$2.log" n chunk rc
  : > "$out/failed-$4-$2.txt"
  : > "$out/passed-$4-$2.txt"
  n=$(wc -l < "$names" | tr -d ' ')
  [ "$n" -gt 0 ] || return 0
  : > "$log"
  rm -rf "$out/chunks"
  mkdir -p "$out/chunks"
  split -l 150 "$names" "$out/chunks/c"
  for chunk in "$out/chunks"/c*; do
    keyring cargo test -p "$pkg" --lib --locked -- --exact $(cat "$chunk") >> "$log" 2>&1
    rc=$?
    echo "  $tag at $at chunk $(basename "$chunk") rc=$rc | $(grep '^test result' "$log" | tail -1 | cut -c1-100)"
  done
  echo "  $tag at $at rows=$n"
  grep -E '^test .* \.\.\. FAILED$' "$log" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' > "$out/failed-$4-$2.txt"
  grep -E '^test .* \.\.\. ok$' "$log" | sed -E 's/^test (.*) \.\.\. ok$/\1/' > "$out/passed-$4-$2.txt"
}
groups() {
  local pkg=$1 at=$2 prefixes=$3 p names n
  : > "$out/all-$pkg-$at.txt"
  for p in $prefixes; do
    names="$out/sel-$pkg-$(printf '%s' "$p" | tr -c 'A-Za-z0-9' _)-$at.txt"
    grep -E "^${p}[^ ]*: test$" "$out/list-$pkg-$at.txt" | sed 's/: test$//' > "$names"
    n=$(wc -l < "$names" | tr -d ' ')
    if [ "$n" -eq 0 ]; then say "rows $pkg $p at $at: ZERO ROWS"; bad=1; continue; fi
    say "rows $pkg $p at $at: $n"
    cat "$names" >> "$out/all-$pkg-$at.txt"
  done
  sort -u -o "$out/all-$pkg-$at.txt" "$out/all-$pkg-$at.txt"
  run_rows "$pkg" "$at" "$out/all-$pkg-$at.txt" "$pkg"
  say "$pkg at $at: $(wc -l < "$out/passed-$pkg-$at.txt" | tr -d ' ') ok, $(wc -l < "$out/failed-$pkg-$at.txt" | tr -d ' ') FAILED of $(wc -l < "$out/all-$pkg-$at.txt" | tr -d ' ')"
}
build_at() {
  local at=$1
  checkout "$at" || return 1
  bins "$at" || return 1
  listed crazyagent "$at" || return 1
  if [ -n "$DESK" ] && ! listed crazyagent-desktop "$at"; then
    say "desktop lib tests did not build at $at"
    echo "$at" >> "$out/desk-red"
  fi
  return 0
}
drive_row() {
  local at=$1 log="$out/drive-$1.log" rc
  keyring cargo test -p crazyagent --lib --locked -- --ignored --exact "$DRIVE" > "$log" 2>&1
  rc=$?
  say "drive-root row at $at rc=$rc: $(grep -E "^test $DRIVE \.\.\. " "$log" | sed 's/.*\.\.\. //')"
  grep -E -A4 'panicked at' "$log" | head -10 | cut -c1-300
}
start=$(date +%s)
checkout "$tip" || exit 1
if [ "$CLIPPY" = 1 ]; then clippy "$tip"; fi
build_at "$tip" || { say "build at $tip NOT"; exit 1; }
say "merge build and clippy in $(( $(date +%s) - start )) s"
pkgs=crazyagent
groups crazyagent "$tip" "$CORE"
if [ -n "$DESK" ] && ! grep -qx "$tip" "$out/desk-red" 2>/dev/null; then groups crazyagent-desktop "$tip" "$DESK"; pkgs="crazyagent crazyagent-desktop"; fi
if grep -qx "$tip" "$out/desk-red" 2>/dev/null; then need_desk=1; bad=1; else need_desk=0; fi
if [ "$os" = windows ]; then
  echo "system drive acl:"; icacls "$SYSTEMDRIVE\\" || true
  echo "node $(command -v node) $(node -v)"
  drive_row "$tip"
fi
need_base=$need_desk
for pkg in $pkgs; do
  if [ -s "$out/failed-$pkg-$tip.txt" ]; then
    run_rows "$pkg" "$tip" "$out/failed-$pkg-$tip.txt" "$pkg-retry"
    while read -r row; do say "FLAKY at merge (failed then ok on retry): $pkg $row"; done < "$out/passed-$pkg-retry-$tip.txt"
    cp "$out/failed-$pkg-retry-$tip.txt" "$out/red-$pkg-merge.txt"
    [ -s "$out/red-$pkg-merge.txt" ] && need_base=1
  fi
done
[ -s "$out/clippy-red-$tip" ] && need_base=1
if [ "$os" = windows ]; then need_base=1; fi
if [ "$need_base" = 1 ]; then
  build_at "$first" || { say "build at $first NOT"; exit 1; }
  if [ -s "$out/clippy-red-$tip" ]; then clippy "$first"; fi
  [ "$os" != windows ] || drive_row "$first"
  if [ "$need_desk" = 1 ]; then
    if grep -qx "$first" "$out/desk-red" 2>/dev/null; then say "desktop lib tests: RED AT BASE $first too"; else say "desktop lib tests: RED ONLY AT MERGE (build at $first)"; fi
  fi
  for pkg in $pkgs; do
    [ -s "$out/red-$pkg-merge.txt" ] || continue
    grep -xF -f "$out/red-$pkg-merge.txt" <(grep ': test$' "$out/list-$pkg-$first.txt" | sed 's/: test$//') > "$out/redsel-$pkg-$first.txt"
    run_rows "$pkg" "$first" "$out/redsel-$pkg-$first.txt" "$pkg-base"
    while read -r row; do
      if grep -qxF "$row" "$out/failed-$pkg-base-$first.txt" 2>/dev/null; then
        say "RED AT BASE $first: $pkg $row"
      elif grep -qxF "$row" "$out/passed-$pkg-base-$first.txt" 2>/dev/null; then
        say "RED ONLY AT MERGE (ok at $first): $pkg $row"
        echo "$row" >> "$out/only-$pkg.txt"
        bad=1
      else
        say "RED AT MERGE, row absent at $first: $pkg $row"
        echo "$row" >> "$out/only-$pkg.txt"
        bad=1
      fi
    done < "$out/red-$pkg-merge.txt"
  done
  second_needed=0
  for pkg in $pkgs; do [ -s "$out/only-$pkg.txt" ] && second_needed=1; done
  if [ "$second_needed" = 1 ]; then
    build_at "$second" || { say "build at $second NOT"; exit 1; }
    for pkg in $pkgs; do
      [ -s "$out/only-$pkg.txt" ] || continue
      grep -xF -f "$out/only-$pkg.txt" <(grep ': test$' "$out/list-$pkg-$second.txt" | sed 's/: test$//') > "$out/redsel-$pkg-$second.txt"
      run_rows "$pkg" "$second" "$out/redsel-$pkg-$second.txt" "$pkg-v243"
      while read -r row; do
        if grep -qxF "$row" "$out/failed-$pkg-v243-$second.txt" 2>/dev/null; then say "  also red at $second (v243's own): $pkg $row"
        elif grep -qxF "$row" "$out/passed-$pkg-v243-$second.txt" 2>/dev/null; then say "  ok at $second: $pkg $row"
        else say "  absent at $second: $pkg $row"; fi
      done < "$out/only-$pkg.txt"
    done
  fi
fi
[ -s "$out/clippy-red-$tip" ] && bad=1
say "end in $(( $(date +%s) - start )) s, bad=$bad"
exit "$bad"
