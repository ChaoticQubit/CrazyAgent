set -uo pipefail
label=${1:?usage: v244-link-cli.sh <label>}
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
out="$RUNNER_TEMP/linkcli"
mkdir -p "$out"
cp .github/farm/v244-path-watch.ps1 "$out/path-watch.ps1"
base=5e25980d6f2f91bd9fe53e54e21cbc8d76c2380b
red=92d07e0b2d2a42eaec8df0ecb4d8f81876affdc6
tip=e54f2788a05bce92761c7c55899164aa505eec4c
pinned=service::tests::windows_path_command_hands_the_folder_over_in_its_environment_only
mine='service(\\|/)tests\.rs|src(\\|/)service\.rs'
rows=(
  service::tests::link_cli_adds_a_folder_named_like_powershell_code_to_the_user_path_once
  service::tests::link_cli_refuses_a_folder_holding_a_semicolon_and_leaves_the_user_path
  service::tests::link_cli_keeps_the_user_path_kind_and_its_unexpanded_references
  "$pinned"
  service::tests::windows_path_command_keeps_the_user_path_kind_and_its_unexpanded_text
)
filter='package(=crazyagent) & kind(lib) & test(/^service::tests::(link_cli_|windows_path_command_)/)'
bad=0
note() {
  printf '%s\n' "$1" >> "$summary"
  printf '%s\n' "$1"
}
checkout() {
  git checkout -q --force --detach "$2" || { note "| $label $1 | checkout $2 FAILED |"; return 1; }
}
built() {
  if cargo nextest run -p crazyagent --lib --locked --profile farm --no-run > "$out/build-$1.txt" 2>&1; then
    return 0
  fi
  note "| $label $1 | lib test build FAILED |"
  tr -d '\r' < "$out/build-$1.txt" | grep -E -A12 '^error' | head -60
  return 1
}
watch_start() {
  rm -f "$out/watch-$1.txt" "$out/stop-$1"
  powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$(cygpath -w "$out/path-watch.ps1")" \
    -Out "$(cygpath -w "$out/watch-$1.txt")" -Stop "$(cygpath -w "$out/stop-$1")" &
  watcher=$!
  for _ in $(seq 1 300); do
    [ -s "$out/watch-$1.txt" ] && return 0
    sleep 0.1
  done
  note "| $label $1 | the PATH watcher never wrote its first value |"
  return 1
}
watch_stop() {
  sleep 2
  : > "$out/stop-$1"
  wait "$watcher"
  tr -d '\r' < "$out/watch-$1.txt" > "$out/values-$1.txt"
  wc -l < "$out/values-$1.txt" | tr -d ' '
}
rounds() {
  local name="$1" count="$2"
  : > "$out/rows-$name.txt"
  for round in $(seq 1 "$count"); do
    echo "== round $round" >> "$out/rows-$name.txt"
    cargo nextest run -p crazyagent --lib --locked --profile farm --no-fail-fast -E "$filter" 2>&1 \
      | tr -d '\r' >> "$out/rows-$name.txt"
  done
}
passed() {
  grep -cE "^[[:space:]]*PASS \[.*[[:space:]]$2\$" "$out/rows-$1.txt"
}
failed() {
  grep -cE "^[[:space:]]*(FAIL|TIMEOUT|SIGSEGV|SIGABRT|ABORT) \[.*[[:space:]]$2\$" "$out/rows-$1.txt"
}
scratch_left() {
  powershell.exe -NoProfile -NonInteractive -Command \
    "@(Get-ChildItem -LiteralPath 'HKCU:\\Software' | Where-Object { \$_.PSChildName -like 'CrazyAgent-test-*' }).Count" \
    | tr -d '\r\n '
}
windows() {
  checkout base "$base" || return 1
  built base || return 1
  watch_start base || return 1
  rounds base 1
  local seen
  seen=$(watch_stop base)
  note "| $label base ${base:0:9} | real user PATH took $seen distinct value(s) while the rows ran (more than 1 = the rows write it) |"
  [ "$seen" -gt 1 ] || bad=1
  checkout tip "$tip" || return 1
  built tip || return 1
  watch_start tip || return 1
  rounds tip 5
  seen=$(watch_stop tip)
  note "| $label tip ${tip:0:9} | real user PATH took $seen distinct value(s) while the rows ran x5 (1 = untouched) |"
  [ "$seen" = 1 ] || bad=1
  local left
  left=$(scratch_left)
  note "| $label tip | scratch keys left under HKCU:\\Software: $left |"
  [ "$left" = 0 ] || bad=1
  note ""
  note "| row at tip | PASS | FAIL |"
  note "| --- | --- | --- |"
  for row in "${rows[@]}"; do
    local ok no
    ok=$(passed tip "$row")
    no=$(failed tip "$row")
    { [ "$ok" = 5 ] && [ "$no" = 0 ]; } || bad=1
    note "| ${row##*::} | $ok | $no |"
  done
}
linux() {
  checkout red "$red" || return 1
  cargo test -p crazyagent --lib --locked --no-run > "$out/build-red.txt" 2>&1 || { note "| $label red | build FAILED |"; return 1; }
  cargo test -p crazyagent --lib --locked --no-fail-fast -- service::tests::windows_path_command > "$out/rows-red.txt" 2>&1
  local r
  r=$(grep -c "^test $pinned \.\.\. FAILED" "$out/rows-red.txt")
  note "| $label red ${red:0:9} | the pinned script row FAILED $r of 1 (1 = red before the fix) |"
  [ "$r" = 1 ] || bad=1
  checkout tip "$tip" || return 1
  cargo test -p crazyagent --lib --locked --no-run > "$out/build-tip.txt" 2>&1 || { note "| $label tip | build FAILED |"; return 1; }
  cargo test -p crazyagent --lib --locked --no-fail-fast -- service::tests::windows_path_command > "$out/rows-tip.txt" 2>&1
  local ok no
  ok=$(grep -c '^test service::tests::windows_path_command_[a-z_]* \.\.\. ok' "$out/rows-tip.txt")
  no=$(grep -c '^test service::tests::windows_path_command_[a-z_]* \.\.\. FAILED' "$out/rows-tip.txt")
  note "| $label tip ${tip:0:9} | windows_path_command rows: $ok ok, $no FAILED (2 ok = green) |"
  { [ "$ok" = 2 ] && [ "$no" = 0 ]; } || bad=1
}
clip() {
  local shown="$1"
  shift
  if cargo clippy -p crazyagent --lib "$@" --locked -- -D warnings > "$out/clippy.txt" 2>&1; then
    note "| $label clippy $shown, -D warnings, at tip | clean |"
    return 0
  fi
  local named
  named=$(tr -d '\r' < "$out/clippy.txt" | grep -cE "$mine")
  [ "$named" = 0 ] || bad=1
  note "| $label clippy $shown, -D warnings, at tip | FAILED, $named line(s) name service.rs or service/tests.rs |"
  tr -d '\r' < "$out/clippy.txt" | grep -E -A8 '^(error|warning)' | head -80
}
lint() {
  clip "lib in test mode" --profile test
  clip lib
}
note "| side | result |"
note "| --- | --- |"
if [ "$RUNNER_OS" = Windows ]; then
  windows || exit 1
else
  linux || exit 1
fi
lint
for name in base red tip; do
  [ -f "$out/rows-$name.txt" ] || continue
  echo "== $name failures =="
  grep -E -B2 -A12 'panicked at' "$out/rows-$name.txt" | head -120
done
note "| verdict $label | $([ "$bad" -eq 0 ] && echo VERIFIED || echo NOT VERIFIED) |"
exit "$bad"
