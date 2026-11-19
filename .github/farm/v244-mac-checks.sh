#!/usr/bin/env bash
set +e -uo pipefail

out="$RUNNER_TEMP/v244-mac-checks-macos.txt"
: > "$out"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }
rows_md="$RUNNER_TEMP/rows.md"
: > "$rows_md"
want=0

row() {
  printf '| %s | %s |\n' "$1" "$2" >> "$rows_md"
  log "$1 rc $2"
}
at() {
  local sha="$1" name="$2"
  [ "${#sha}" = 40 ] || { row "$name sha $sha is not a full sha" 1; return 1; }
  git cat-file -e "$sha^{commit}" 2>/dev/null || git fetch -q --no-tags --depth=1 origin "$sha" \
    || { row "$name fetch ${sha:0:9}" "$?"; return 1; }
  git checkout -q --force --detach "$sha" || { row "$name checkout ${sha:0:9}" "$?"; return 1; }
  [ "$(git rev-parse HEAD)" = "$(git rev-parse "$sha^{commit}")" ] || { row "$name HEAD is ${sha:0:9}" 1; return 1; }
  mkdir -p apps/desktop/dist
  [ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
  log "== $name at $(git rev-parse --short=9 HEAD), Cargo.lock $(git rev-parse --short=7 HEAD:Cargo.lock)"
  cargo fetch --locked > /dev/null 2>&1
}
step() {
  local name="$1" need="$2"
  shift 2
  "$@" 2>&1 | tee -a "$out" > /dev/null
  local rc=${PIPESTATUS[0]}
  row "$name" "$rc"
  [ "$need" = any ] || [ "$rc" = "$need" ] || want=1
}
clippy() { cargo clippy "${pkg[@]}" --all-targets -- -D warnings; }
wd_clippy() { cargo clippy "${pkg[@]}" --all-targets --features native-webdriver -- -D warnings; }
rows() {
  cargo test "${pkg[@]}" --lib --no-fail-fast "$@" > "$RUNNER_TEMP/rows.txt" 2>&1
  local rc=$?
  grep -E '^test result|FAILED|panicked' "$RUNNER_TEMP/rows.txt"
  return "$rc"
}
same() {
  local base="$1"
  [ "${#base}" = 40 ] || return 3
  git cat-file -e "$base^{commit}" 2>/dev/null || git fetch -q --no-tags --depth=1 origin "$base" || return 2
  git diff --quiet "$base" HEAD -- Cargo.lock Cargo.toml apps/desktop/src-tauri/Cargo.toml
}
red_import() {
  cargo check "${pkg[@]}" --lib > "$RUNNER_TEMP/red-import.txt" 2>&1
  local rc=$?
  local hits
  hits=$(grep -c 'cannot find value .SKIPPED.' "$RUNNER_TEMP/red-import.txt")
  log "seam red check rc $rc, E0425 SKIPPED hits $hits"
  grep -E '^error' -A3 "$RUNNER_TEMP/red-import.txt" | head -n 12
  [ "$rc" -ne 0 ] && [ "$hits" -ge 1 ]
}

log "os macos $(sw_vers -productVersion) $(rustc -V) cores $(sysctl -n hw.ncpu)"
if at a54cd7ffba07862ec1dc1d08d3ca93fc3a945aa5 host; then
  step "host clippy" 0 clippy
  step "host native-webdriver clippy" 0 wd_clippy
  step "host native_term rows" 0 rows -- native_term
  step "host lock and manifests same since 96e12e0ba" 0 same 96e12e0ba647b26ed68702c7e2d347cc92040b0b
else want=1; fi
if at 4d30f24ee7d88a676608ed882236592fc1be3da7 device-lost; then
  step "device-lost metadata --locked" 0 cargo metadata --locked --format-version 1
  step "device-lost lock and manifests same since 27ff30baf" 0 same 27ff30bafdc48cf3e9635f7e99e910dd64c7ff74
  step "device-lost clippy" 0 clippy
else want=1; fi
if at a51e0fa5150be641b7e72aaa825fb16545164eaa seam; then
  step "seam clippy" 0 clippy
  step "seam native-webdriver clippy" 0 wd_clippy
  step "seam native_term rows" 0 rows -- native_term
else want=1; fi
if at 6cfa8c202560fb0a2f4b04b4f87b3616969df266 seam-red; then
  step "seam red at 6cfa8c202, default check fails with E0425 SKIPPED (want 0)" 0 red_import
else want=1; fi
if at 99feff82c427939942c3c86a0f2c110ba157daca librs; then
  step "librs clippy" 0 clippy
  step "librs native-webdriver clippy" 0 wd_clippy
  step "librs lib rows" 0 rows
  step "librs lock and manifests same since 96e12e0ba" 0 same 96e12e0ba647b26ed68702c7e2d347cc92040b0b
else want=1; fi
if at 0ff0fd91bd7ce145749e6a35f414d847d8aed1af mouse; then
  step "mouse clippy" 0 clippy
  step "mouse native-webdriver clippy" 0 wd_clippy
  step "mouse native_term rows" 0 rows -- native_term
  step "mouse lock and manifests same since 66102f50f" 0 same 66102f50fa25e4cf9c67ee76a7222105a466613c
else want=1; fi

{
  printf '| macos-15 step | exit |\n| --- | --- |\n'
  cat "$rows_md"
  printf '| every step as wanted (0 = yes) | %s |\n' "$want"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"
[ "$want" = 0 ]
