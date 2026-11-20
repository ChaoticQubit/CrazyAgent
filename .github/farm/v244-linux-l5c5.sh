#!/usr/bin/env bash
set +e -uo pipefail

os="$1"
case "$os" in
  macos | linux | windows) ;;
  *) echo "::error::unknown os $os"; exit 2 ;;
esac
out="$RUNNER_TEMP/v244-linux-l5c5-$os.txt"
: > "$out"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }

shared_rows=(
  native_term::render::tests::every_style_finds_its_own_face_with_no_system_font
  native_term::render::tests::each_platform_draws_with_its_own_backends
  native_term::render::tests::marked_text_is_underlined_along_the_bottom_of_its_cells
  native_term::tests::no_native_file_brings_the_app_forward_or_makes_a_window_key
  native_term::tests::the_focus_scan_finds_a_planted_call
  native_term::tests::no_host_platform_file_brings_the_app_forward_or_makes_a_window_key
  native_term::tests::only_the_page_copy_command_writes_a_board_with_no_user_event
  native_term::host::tests::a_style_or_command_off_the_main_thread_is_refused_and_never_panics
  tests::every_native_host_use_builds_on_linux_and_native_stays_off_there
  native_term::ime::tests::keys_route_by_modifiers_and_composition
  native_term::a11y::tests::a_selection_ending_on_a_wide_characters_left_cell_keeps_it
  native_term::screen::tests::marked_text_replaces_cells_from_the_cursor_and_changes_only_its_row_key
)
case "$os" in
  macos)
    os_rows=(
      native_term::render::tests::a_frame_on_a_lost_device_reports_the_gpu_code_before_glyphon_maps_a_buffer
      native_term::render::tests::a_surface_past_the_device_texture_limit_is_held_to_it_and_still_draws
      native_term::render::tests::menlo_loads_from_its_system_file
      native_term::tests::engine_socket_needs_an_engine_db_and_a_socket
    )
    ;;
  linux)
    os_rows=(
      native_term::style::tests::on_linux_monospace_is_dejavu_sans_mono_else_the_built_in_font
      native_term::render::tests::the_built_in_font_gives_the_rig_xterm_grid
      native_term::tests::engine_socket_needs_an_engine_db_and_a_socket
    )
    ;;
  windows)
    os_rows=()
    ;;
esac

mkdir -p apps/desktop/dist
[ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
log "os $os base $BASE_SHA red $RED_SHA tip $TIP_SHA $(rustc -V)"
git merge-base --is-ancestor "$BASE_SHA" HEAD
anc=$?
log "base $BASE_SHA is in HEAD (0 = yes): $anc"

log "== lib build"
cargo build "${pkg[@]}" --lib 2>&1 | tee -a "$out"
lib_built=${PIPESTATUS[0]}
log "== lib rows build"
cargo test "${pkg[@]}" --lib --no-run 2>&1 | tee -a "$out"
rows_built=${PIPESTATUS[0]}

rows_rc=na
want=0
passed=na
if [ "$rows_built" = 0 ]; then
  log "== native_term rows and the lib.rs native host scan"
  cargo test "${pkg[@]}" --lib --no-fail-fast -- native_term tests::every_native_host_use_builds_on_linux_and_native_stays_off_there 2>&1 | tee "$RUNNER_TEMP/rows.txt" >> "$out"
  rows_rc=${PIPESTATUS[0]}
  passed=$(grep -cE '^test .* \.\.\. ok$' "$RUNNER_TEMP/rows.txt")
  failed=$(awk '/^test .* \.\.\. FAILED$/ {print $2}' "$RUNNER_TEMP/rows.txt" | sort -u)
  [ -z "$failed" ] || { log "::error::rows failed:"; log "$failed"; want=1; }
  for row in "${shared_rows[@]}" "${os_rows[@]}"; do
    grep -qx "test $row ... ok" "$RUNNER_TEMP/rows.txt" || { log "::error::$row did not pass"; want=1; }
  done
  grep -E '^test result:' "$RUNNER_TEMP/rows.txt" | tee -a "$out"
else
  want=1
fi

log "== clippy --lib"
cargo clippy "${pkg[@]}" --lib -- -D warnings 2>&1 | tee -a "$out"
clip=${PIPESTATUS[0]}
log "== clippy --tests"
cargo clippy "${pkg[@]}" --tests -- -D warnings 2>&1 | tee -a "$out"
clipt=${PIPESTATUS[0]}

wd=na
wdt=na
if [ "$os" = macos ]; then
  log "== clippy --lib --features native-webdriver"
  cargo clippy "${pkg[@]}" --lib --features native-webdriver -- -D warnings 2>&1 | tee -a "$out"
  wd=${PIPESTATUS[0]}
  log "== clippy --tests --features native-webdriver"
  cargo clippy "${pkg[@]}" --tests --features native-webdriver -- -D warnings 2>&1 | tee -a "$out"
  wdt=${PIPESTATUS[0]}
fi

red_ok=na
if [ "$rows_built" = 0 ]; then
  red_ok=0
  src=apps/desktop/src-tauri/src
  log "== red: $src as at $RED_SHA; each L5c-5 row must fail by its own assertion"
  git checkout -q "$RED_SHA" -- "$src"
  git diff --cached --stat | tee -a "$out"
  host_row=native_term::host::tests::a_style_or_command_off_the_main_thread_is_refused_and_never_panics
  scan_row=tests::every_native_host_use_builds_on_linux_and_native_stays_off_there
  for pair in "$host_row|a style or command off the main thread never panics" "$scan_row|uses the native host under"; do
    row=${pair%%|*}
    message=${pair#*|}
    cargo test "${pkg[@]}" --lib -- --exact "$row" > "$RUNNER_TEMP/red.txt" 2>&1
    rc=$?
    tail -n 25 "$RUNNER_TEMP/red.txt" >> "$out"
    own=1
    if [ "$rc" = 101 ] && grep -qxF "test $row ... FAILED" "$RUNNER_TEMP/red.txt" && grep -qF "$message" "$RUNNER_TEMP/red.txt"; then
      own=0
    fi
    log "red $row exit $rc, own assertion (0 = yes) $own"
    [ "$own" = 0 ] || red_ok=1
  done
  git checkout -q HEAD -- "$src"
  [ -z "$(git status --porcelain --untracked-files=no)" ] || { log "::error::the red left the tree changed"; red_ok=1; }
fi

{
  printf '| %s | exit |\n| --- | --- |\n' "$os"
  printf '| base %s in HEAD | %s |\n' "$BASE_SHA" "$anc"
  printf '| lib build | %s |\n| lib rows build | %s |\n' "$lib_built" "$rows_built"
  printf '| native_term rows and the lib.rs scan | %s, %s passed |\n' "$rows_rc" "$passed"
  printf '| named rows ok, none failed (0 = yes) | %s |\n' "$want"
  printf '| clippy --lib -D warnings | %s |\n| clippy --tests -D warnings | %s |\n' "$clip" "$clipt"
  printf '| clippy --lib --features native-webdriver -D warnings (macOS only) | %s |\n' "$wd"
  printf '| clippy --tests --features native-webdriver -D warnings (macOS only) | %s |\n' "$wdt"
  printf '| red %s: both L5c-5 rows fail by their own assertion (0 = yes) | %s |\n' "$RED_SHA" "$red_ok"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$anc" = 0 ] && [ "$lib_built" = 0 ] && [ "$rows_built" = 0 ] && [ "$want" = 0 ] \
  && [ "$rows_rc" = 0 ] && [ "$clip" = 0 ] && [ "$clipt" = 0 ] \
  && { { [ "$wd" = 0 ] && [ "$wdt" = 0 ]; } || [ "$os" != macos ]; } && [ "$red_ok" = 0 ]
