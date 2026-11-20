#!/usr/bin/env bash
set +e -uo pipefail

image="${ImageOS:-unknown}"
out="$RUNNER_TEMP/v244-h5c-a-$image.txt"
: > "$out"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }
kit="$RUNNER_TEMP/h5c-a"
lib_rs=apps/desktop/src-tauri/src/lib.rs
conf=test/native/native-terminal-windows.wdio.conf.mjs
sweep() {
  local name
  for name in crazyagent-desktop.exe msedgewebview2.exe; do
    taskkill //F //T //IM "$name" > /dev/null 2>&1 || true
  done
}
wdio() {
  local name="$1" port="$2"
  (cd apps/desktop && WD_APP="$exe" WD_PORT="$port" timeout 600 npx wdio run "$conf" > "$RUNNER_TEMP/$name.txt" 2>&1)
  local rc=$?
  grep -E 'h5c row:|passing|failing|AssertionError|Error:|✓|✖' "$RUNNER_TEMP/$name.txt" | head -n 60 | tee -a "$out"
  sweep
  return "$rc"
}
count() {
  local what="$1" file="$2" n
  n=$(grep -oE "[0-9]+ $what" "$file" | tail -n 1 | awk '{print $1}')
  printf '%s' "${n:-0}"
}

log "image $image green $GREEN_SHA $(rustc -V) node $(node -v 2>&1) npm $(npm -v 2>&1)"
runtime=$(reg query 'HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}' //v pv 2>/dev/null | awk '/pv/ {print $3}')
log "webview2 runtime ${runtime:-none}"

log "== display, before the app starts"
powershell -NoProfile -Command 'Add-Type -AssemblyName System.Windows.Forms; foreach ($s in [System.Windows.Forms.Screen]::AllScreens) { "display {0} primary {1} bounds {2}x{3} work {4}x{5} bits {6}" -f $s.DeviceName, $s.Primary, $s.Bounds.Width, $s.Bounds.Height, $s.WorkingArea.Width, $s.WorkingArea.Height, $s.BitsPerPixel }' 2>&1 | tr -d '\r' | tee -a "$out"
display_rc=${PIPESTATUS[0]}
powershell -NoProfile -Command 'Get-CimInstance Win32_VideoController | ForEach-Object { "video {0} {1}x{2}" -f $_.Name, $_.CurrentHorizontalResolution, $_.CurrentVerticalResolution }' 2>&1 | tr -d '\r' | tee -a "$out"

log "== the real frontend: npm ci and npm run build"
(cd apps/desktop && npm ci --no-audit --no-fund > "$RUNNER_TEMP/npm-ci.txt" 2>&1)
ci_rc=$?
tail -n 5 "$RUNNER_TEMP/npm-ci.txt" | tee -a "$out"
front_rc=na
if [ "$ci_rc" = 0 ]; then
  (cd apps/desktop && npm run build > "$RUNNER_TEMP/front-build.txt" 2>&1)
  front_rc=$?
  tail -n 8 "$RUNNER_TEMP/front-build.txt" | tee -a "$out"
fi
title=0
grep -q '<title>CrazyAgent</title>' apps/desktop/dist/index.html 2>/dev/null && title=1
log "dist index.html is the real frontend (1 = yes): $title"

built=na
green_rc=na
green_pass=na
green_fail=na
red_built=na
red_rc=na
red_fail=na
red_own=0
if [ "$front_rc" = 0 ] && [ "$title" = 1 ]; then
  log "== build: crazyagent-desktop with native-webdriver at $GREEN_SHA"
  cargo build "${pkg[@]}" --features native-webdriver --bin crazyagent-desktop > "$RUNNER_TEMP/wd-build.txt" 2>&1
  built=$?
  grep -E '^error' -A8 "$RUNNER_TEMP/wd-build.txt" | head -n 80 | tee -a "$out"
  tail -n 3 "$RUNNER_TEMP/wd-build.txt" | tee -a "$out"
  exe="$(cygpath -w "$PWD/target/debug/crazyagent-desktop.exe")"
  if [ "$built" = 0 ]; then
    log "== green: the Windows suite on the real app (want 3 passing, 0 failing)"
    wdio green-wdio 4446
    green_rc=$?
    green_pass=$(count passing "$RUNNER_TEMP/green-wdio.txt")
    green_fail=$(count failing "$RUNNER_TEMP/green-wdio.txt")
    log "green: log tail"
    tail -n 30 "$RUNNER_TEMP/green-wdio.txt" | tee -a "$out"

    log "== red: native_enabled on Windows (farm only, never merged), want the enabled row to fail on its own assertion"
    git apply "$kit/red-flag-on.patch"
    applied=$?
    git diff --stat -- "$lib_rs" | tee -a "$out"
    if [ "$applied" = 0 ]; then
      cargo build "${pkg[@]}" --features native-webdriver --bin crazyagent-desktop > "$RUNNER_TEMP/red-build.txt" 2>&1
      red_built=$?
      tail -n 3 "$RUNNER_TEMP/red-build.txt" | tee -a "$out"
      if [ "$red_built" = 0 ]; then
        wdio red-wdio 4447
        red_rc=$?
        red_fail=$(count failing "$RUNNER_TEMP/red-wdio.txt")
        grep -Fq 'Windows native views stay off until the flag' "$RUNNER_TEMP/red-wdio.txt" && red_own=1
      fi
    fi
    git checkout -- "$lib_rs"
    git diff --quiet || { log "::error::the tree is not clean after the red leg"; red_own=0; }
  fi
fi

{
  printf '| windows %s | exit |\n| --- | --- |\n' "$image"
  printf '| WebView2 runtime | %s |\n' "${runtime:-none}"
  printf '| display probe (lines above) | %s |\n' "$display_rc"
  printf '| npm ci, real frontend build, dist is the real frontend (want 0, 0, 1) | %s, %s, %s |\n' "$ci_rc" "$front_rc" "$title"
  printf '| native-webdriver app build | %s |\n' "$built"
  printf '| green suite rc, passing, failing (want 0, 3, 0) | %s, %s, %s |\n' "$green_rc" "$green_pass" "$green_fail"
  printf '| red build, red suite rc, failing, failed on its own assertion (want 0, non-zero, >=1, 1) | %s, %s, %s, %s |\n' \
    "$red_built" "$red_rc" "$red_fail" "$red_own"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$built" = 0 ] && [ "$green_rc" = 0 ] && [ "$green_pass" = 3 ] && [ "$green_fail" = 0 ] \
  && [ "$red_built" = 0 ] && [ "$red_rc" != 0 ] && [ "$red_own" = 1 ]
