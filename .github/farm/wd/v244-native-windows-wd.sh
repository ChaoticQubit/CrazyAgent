#!/usr/bin/env bash
set +e -uo pipefail

image="${ImageOS:-unknown}"
out="$RUNNER_TEMP/v244-native-windows-wd-$image.txt"
: > "$out"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }
probe="$RUNNER_TEMP/wd"
sweep() {
  local name
  for name in crazyagent-desktop.exe msedgewebview2.exe msedgedriver.exe tauri-driver.exe; do
    taskkill //F //T //IM "$name" > /dev/null 2>&1 || true
  done
}

log "image $image green $GREEN_SHA $(rustc -V) node $(node -v 2>&1) npm $(npm -v 2>&1)"
runtime=$(reg query 'HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}' //v pv 2>/dev/null | awk '/pv/ {print $3}')
log "webview2 runtime ${runtime:-none}"
log "image msedgedriver ${EdgeWebDriver:-none} $("${EdgeWebDriver:-none}/msedgedriver.exe" --version 2>&1 | head -n 1)"

mkdir -p apps/desktop/dist
cp "$probe/index.html" "$probe/probe.js" apps/desktop/dist/
log "== build: crazyagent-desktop with native-webdriver"
cargo build "${pkg[@]}" --features native-webdriver --bin crazyagent-desktop > "$RUNNER_TEMP/wd-build.txt" 2>&1
built=$?
grep -E '^error' -A8 "$RUNNER_TEMP/wd-build.txt" | head -n 160 | tee -a "$out"
tail -n 5 "$RUNNER_TEMP/wd-build.txt" | tee -a "$out"
exe="$(cygpath -w "$PWD/target/debug/crazyagent-desktop.exe")"
drove=native-webdriver
plain_built=na
if [ "$built" != 0 ]; then
  log "== fallback: the plain app build, for the tauri-driver leg"
  cargo build "${pkg[@]}" --bin crazyagent-desktop > "$RUNNER_TEMP/plain-build.txt" 2>&1
  plain_built=$?
  tail -n 5 "$RUNNER_TEMP/plain-build.txt" | tee -a "$out"
  drove=plain
fi

raw_rc=na
ci_rc=na
wdio_rc=na
if [ "$built" = 0 ]; then
  log "== raw: W3C calls against the app's embedded server"
  timeout 420 python -I "$probe/webdriver_probe.py" embedded "$exe" 4445 "$(cygpath -w "$RUNNER_TEMP/raw-app.txt")" > "$RUNNER_TEMP/raw.txt" 2>&1
  raw_rc=$?
  grep -E '^wd:|Traceback|Error' "$RUNNER_TEMP/raw.txt" | head -n 60 | tee -a "$out"
  log "raw: app log tail"
  tail -n 20 "$RUNNER_TEMP/raw-app.txt" | tee -a "$out"
  sweep
  log "== wdio: the repo's @wdio/tauri-service, embedded provider"
  (cd apps/desktop && npm ci --no-audit --no-fund > "$RUNNER_TEMP/npm-ci.txt" 2>&1)
  ci_rc=$?
  tail -n 10 "$RUNNER_TEMP/npm-ci.txt" | tee -a "$out"
  if [ "$ci_rc" = 0 ]; then
    mkdir -p apps/desktop/test/farm-probe
    cp "$probe/probe.wdio.conf.mjs" "$probe/probe.e2e.mjs" apps/desktop/test/farm-probe/
    (cd apps/desktop && WD_APP="$exe" WD_PORT=4446 timeout 400 npx wdio run test/farm-probe/probe.wdio.conf.mjs > "$RUNNER_TEMP/wdio.txt" 2>&1)
    wdio_rc=$?
    grep -E 'wdio:|passing|failing|Error|assert|✓|✖' "$RUNNER_TEMP/wdio.txt" | head -n 60 | tee -a "$out"
    log "wdio: log tail"
    tail -n 40 "$RUNNER_TEMP/wdio.txt" | tee -a "$out"
  fi
fi

edge_rc=na
td_rc=na
official_rc=na
if { [ "$built" = 0 ] || [ "$plain_built" = 0 ]; } && [ -n "$runtime" ]; then
  sweep
  log "== official: tauri-driver with msedgedriver $runtime, on the $drove build"
  mkdir -p "$RUNNER_TEMP/edge"
  curl -fsSL -o "$RUNNER_TEMP/edge.zip" "https://msedgedriver.microsoft.com/$runtime/edgedriver_win64.zip"
  edge_rc=$?
  if [ "$edge_rc" != 0 ]; then
    curl -fsSL -o "$RUNNER_TEMP/edge.zip" "https://msedgedriver.azureedge.net/$runtime/edgedriver_win64.zip"
    edge_rc=$?
  fi
  [ "$edge_rc" = 0 ] && { unzip -oq "$RUNNER_TEMP/edge.zip" -d "$RUNNER_TEMP/edge"; edge_rc=$?; }
  log "msedgedriver download and unzip rc $edge_rc"
  cargo install tauri-driver --locked --root "$RUNNER_TEMP/td" > "$RUNNER_TEMP/td.txt" 2>&1
  td_rc=$?
  grep -E 'Installing|Installed|error' "$RUNNER_TEMP/td.txt" | head -n 10 | tee -a "$out"
  log "msedgedriver $("$RUNNER_TEMP/edge/msedgedriver.exe" --version 2>&1 | head -n 1)"
  if [ "$edge_rc" = 0 ] && [ "$td_rc" = 0 ]; then
    timeout 420 python -I "$probe/webdriver_probe.py" official "$exe" "$(cygpath -w "$RUNNER_TEMP/td/bin/tauri-driver.exe")" "$(cygpath -w "$RUNNER_TEMP/edge/msedgedriver.exe")" 4447 "$(cygpath -w "$RUNNER_TEMP/official-driver.txt")" > "$RUNNER_TEMP/official.txt" 2>&1
    official_rc=$?
    grep -E '^wd:|Traceback|Error' "$RUNNER_TEMP/official.txt" | head -n 60 | tee -a "$out"
    log "official: driver log tail"
    tail -n 20 "$RUNNER_TEMP/official-driver.txt" | tee -a "$out"
  fi
  sweep
fi

{
  printf '| windows %s | exit |\n| --- | --- |\n' "$image"
  printf '| WebView2 runtime | %s |\n' "${runtime:-none}"
  printf '| native-webdriver app build, plain fallback build | %s, %s |\n' "$built" "$plain_built"
  printf '| raw W3C on the embedded server (0 ok, 5 no server, 6 no session, 7 a step) | %s |\n' "$raw_rc"
  printf '| npm ci, wdio tauri-service embedded run | %s, %s |\n' "$ci_rc" "$wdio_rc"
  printf '| msedgedriver, tauri-driver install, official raw W3C on the %s build (informational) | %s, %s, %s |\n' \
    "$drove" "$edge_rc" "$td_rc" "$official_rc"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$built" = 0 ] && [ "$raw_rc" = 0 ] && [ "$wdio_rc" = 0 ]
