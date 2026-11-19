#!/usr/bin/env bash
set +e -uo pipefail

os="$1"
case "$os" in
  linux) ;;
  *) echo "::error::the surface clip rows run on Linux only, not $os"; exit 2 ;;
esac
out="$RUNNER_TEMP/v244-linux-l5c-$os.txt"
: > "$out"
root="$RUNNER_TEMP/l5c-cells"
mkdir -p "$root"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }

clip_row=native_term::linux::surface::tests::a_child_shows_only_its_part_inside_the_webview_and_reclips_on_size_allocate
timeout_row=native_term::linux::clip::tests::a_board_call_from_a_timeout_is_refused
focus_row=native_term::linux::term::tests::focus_asked_before_the_first_place_is_applied_right_after_it
cover_row=native_term::linux::term::tests::a_cover_takes_the_child_off_screen_until_the_next_shown_place
rescale_row=native_term::linux::term::tests::a_rescale_reports_the_new_scale_and_the_next_present_carries_it
focus_message="focus asked before the first place is applied right after it"
cover_message="a covered term's child never presents"
rescale_message="a place at a new scale reports it, so the host remakes the surface"
red_rows=(
  native_term::linux::surface::tests::the_visible_part_is_the_rect_inside_the_webview
  native_term::linux::surface::tests::a_child_with_no_visible_part_never_presents_and_owes_a_frame_when_one_comes_back
  native_term::linux::clip::tests::only_a_key_or_button_event_reads_or_writes_a_board
  native_term::linux::clip::tests::a_read_longer_than_a_copy_is_dropped
  native_term::linux::clip::tests::a_private_board_keeps_each_board_in_process
  native_term::linux::surface::tests::a_host_rect_is_finite_bounded_and_at_least_one_pixel
  native_term::linux::surface::tests::device_pixels_scale_the_logical_rect
  native_term::linux::surface::tests::a_scale_out_of_range_is_a_bug
  native_term::linux::surface::tests::a_child_presents_only_with_a_change_and_no_frame_callback_owed
  native_term::linux::surface::tests::a_hidden_child_never_presents_and_showing_redraws_it
  native_term::linux::surface::tests::a_present_that_did_not_happen_is_owed_again
  native_term::linux::surface::tests::a_child_whose_toplevel_unmapped_never_presents_again
  native_term::linux::surface::tests::presenting_when_not_ready_is_a_bug
)
display_row=native_term::linux::surface::tests::a_child_places_hides_shows_and_presents_only_on_its_frame_callback
kept_rows=(
  native_term::linux::keys::tests::letters_carry_control_and_alt_as_xterm_bytes
  native_term::linux::keys::tests::super_and_hyper_chords_send_nothing
  native_term::linux::keys::tests::edit_keys_follow_the_panel
  native_term::keymap::tests::named_keys_carry_their_modifiers
  native_term::render::tests::each_platform_draws_with_its_own_backends
  native_term::style::tests::on_linux_monospace_is_dejavu_sans_mono_else_the_built_in_font
  native_term::render::tests::every_style_finds_its_own_face_with_no_system_font
  native_term::render::tests::marked_text_is_underlined_along_the_bottom_of_its_cells
  native_term::render::tests::the_built_in_font_gives_the_rig_xterm_grid
  native_term::tests::no_native_file_brings_the_app_forward_or_makes_a_window_key
  native_term::tests::the_focus_scan_finds_a_planted_call
  native_term::tests::no_linux_native_file_raises_or_focuses_a_window
  native_term::tests::engine_socket_needs_an_engine_db_and_a_socket
  native_term::ime::tests::keys_route_by_modifiers_and_composition
  native_term::a11y::tests::a_selection_ending_on_a_wide_characters_left_cell_keeps_it
  native_term::screen::tests::marked_text_replaces_cells_from_the_cursor_and_changes_only_its_row_key
  native_term::pane::tests::a_pane_opened_with_keys_off_sends_nothing_from_its_first_key_click_or_paste
  native_term::pane::tests::a_seed_flattens_line_breaks_unless_the_pane_reads_pastes_bracketed
)

mkdir -p apps/desktop/dist
[ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
log "os $os base $BASE_SHA red $L5C_RED_SHA green $GREEN_SHA $(rustc -V)"
log "weston $(weston --version 2>&1 | head -1); $(sway --version 2>&1 | head -1); $(vulkaninfo --summary 2>/dev/null | grep -m1 deviceName)"
git merge-base --is-ancestor "$BASE_SHA" "$L5C_RED_SHA" && git merge-base --is-ancestor "$L5C_RED_SHA" "$GREEN_SHA"
anc=$?
log "base $BASE_SHA, red $L5C_RED_SHA and green $GREEN_SHA in line (0 = yes): $anc"
git checkout -q --detach "$GREEN_SHA"

log "== green $GREEN_SHA: lib build"
cargo build "${pkg[@]}" --lib 2>&1 | tee -a "$out"
lib_built=${PIPESTATUS[0]}
log "== green: lib rows build"
cargo test "${pkg[@]}" --lib --no-run 2>&1 | tee -a "$out"
rows_built=${PIPESTATUS[0]}

rows_rc=na
want=0
passed=na
if [ "$rows_built" = 0 ]; then
  log "== green: native_term rows"
  cargo test "${pkg[@]}" --lib --no-fail-fast -- native_term 2>&1 | tee "$RUNNER_TEMP/rows.txt" >> "$out"
  rows_rc=${PIPESTATUS[0]}
  passed=$(grep -cE '^test native_term::.* \.\.\. ok$' "$RUNNER_TEMP/rows.txt")
  failed=$(awk '/^test .* \.\.\. FAILED$/ {print $2}' "$RUNNER_TEMP/rows.txt" | sort -u)
  [ -z "$failed" ] || { log "::error::rows failed:"; log "$failed"; want=1; }
  for row in "${red_rows[@]}" "${kept_rows[@]}"; do
    grep -qE "^test $row (- should panic )?\.\.\. ok$" "$RUNNER_TEMP/rows.txt" || { log "::error::$row did not pass"; want=1; }
  done
  for row in "$display_row" "$clip_row"; do
    grep -qx "test $row ... ignored, needs a display: the farm runs it under Xvfb, weston and sway" "$RUNNER_TEMP/rows.txt" \
      || { log "::error::$row is not ignored in a plain run"; want=1; }
  done
  for row in "$timeout_row" "$focus_row"; do
    grep -qx "test $row ... ignored, needs a display: the farm runs it under Xvfb and sway" "$RUNNER_TEMP/rows.txt" \
      || { log "::error::$row is not ignored in a plain run"; want=1; }
  done
  for row in "$cover_row" "$rescale_row"; do
    grep -qx "test $row ... ignored, needs a display: the farm runs it under Xvfb, weston and sway" "$RUNNER_TEMP/rows.txt" \
      || { log "::error::$row is not ignored in a plain run"; want=1; }
  done
  grep -E '^test result:' "$RUNNER_TEMP/rows.txt" | tee -a "$out"
fi

display_pid=""
load_pids=()
cell=""

start_x11() {
  Xvfb :99 -screen 0 1280x800x24 -nolisten tcp -noreset > "$root/xvfb-$cell.log" 2>&1 &
  display_pid=$!
  export DISPLAY=:99 GDK_BACKEND=x11
  unset WAYLAND_DISPLAY
  for _ in $(seq 1 50); do
    [ -S /tmp/.X11-unix/X99 ] && return 0
    sleep 0.1
  done
  return 1
}

start_weston() {
  export XDG_RUNTIME_DIR="$RUNNER_TEMP/xdg-$cell" GDK_BACKEND=wayland
  mkdir -p -m 700 "$XDG_RUNTIME_DIR"
  weston --socket=wayland-farm --width=1280 --height=800 --idle-time=0 --debug --backend=headless --renderer=pixman --shell=kiosk > "$root/weston-$cell.log" 2>&1 &
  display_pid=$!
  export WAYLAND_DISPLAY=wayland-farm
  unset DISPLAY
  for _ in $(seq 1 50); do
    [ -S "$XDG_RUNTIME_DIR/wayland-farm" ] && return 0
    sleep 0.1
  done
  return 1
}

start_sway() {
  export XDG_RUNTIME_DIR="$RUNNER_TEMP/xdg-$cell" GDK_BACKEND=wayland
  mkdir -p -m 700 "$XDG_RUNTIME_DIR"
  printf '%s\n' 'output HEADLESS-1 resolution 1280x800 position 0 0' 'default_border none' 'default_floating_border none' 'focus_follows_mouse no' 'xwayland disable' > "$root/sway.conf"
  WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman WLR_HEADLESS_OUTPUTS=1 sway -c "$root/sway.conf" > "$root/sway-$cell.log" 2>&1 &
  display_pid=$!
  unset DISPLAY
  local socket
  for _ in $(seq 1 50); do
    socket=$(ls "$XDG_RUNTIME_DIR" 2> /dev/null | grep -E '^wayland-[0-9]+$' | head -1)
    if [ -n "$socket" ]; then
      export WAYLAND_DISPLAY=$socket
      return 0
    fi
    sleep 0.1
  done
  return 1
}

stop_display() {
  [ -n "$display_pid" ] && kill "$display_pid" 2> /dev/null
  wait "$display_pid" 2> /dev/null
  display_pid=""
}

load_on() {
  for _ in $(seq 1 "$(nproc)"); do
    yes > /dev/null &
    load_pids+=($!)
  done
}

load_off() {
  [ "${#load_pids[@]}" = 0 ] && return
  kill "${load_pids[@]}" 2> /dev/null
  wait "${load_pids[@]}" 2> /dev/null
  load_pids=()
}

shot() {
  local kind=$1 file=$2
  case "$kind" in
    x11) xwd -root -silent > "$file.xwd" && convert "xwd:$file.xwd" "$file" ;;
    sway) timeout 10 grim "$file" ;;
    weston)
      local shots="$root/shots"
      mkdir -p "$shots"
      rm -f "$shots"/*.png
      (cd "$shots" && timeout 10 weston-screenshooter > /dev/null 2>&1) || return 1
      local taken
      taken=$(ls "$shots"/*.png 2> /dev/null | head -1)
      [ -n "$taken" ] && mv "$taken" "$file"
      ;;
  esac
}

probe() {
  local name=$1 file=$2 x=$3 y=$4 want=$5 got
  got=$(convert "$file" -crop "1x1+${x}+${y}" +repage -format '%[fx:int(255*r+0.5)],%[fx:int(255*g+0.5)],%[fx:int(255*b+0.5)]' info: 2> /dev/null || echo unreadable)
  if [ "$got" = "$want" ]; then
    log "  $cell $name ($x,$y) $got ok"
    return 0
  fi
  log "::error::$cell $name ($x,$y) got $got, want $want"
  return 1
}

pixels() {
  local kind=$1 step=$2 dir=$3
  local file="$dir/$step.png"
  shot "$kind" "$file" || { log "::error::$cell $step: no screenshot"; return 1; }
  local ox oy w h s
  read -r ox oy w h s < <(jq -r '[(.root[0] // .device[0]), (.root[1] // .device[1]), .device[2], .device[3], .scale] | @tsv' "$dir/$step.json")
  local cx=$((ox + w / 2)) cy=$((oy + h / 2)) px=$((ox - 15 * s)) py=$((oy + 10 * s)) rc=0
  case "$step" in
    placed | shown)
      probe "${step}_child" "$file" "$cx" "$cy" 0,255,0 || rc=1
      probe "${step}_page" "$file" "$px" "$py" 255,255,255 || rc=1
      ;;
    hidden | covered)
      probe "${step}_child_area" "$file" "$cx" "$cy" 255,255,255 || rc=1
      ;;
    uncovered)
      probe uncovered_child "$file" "$cx" "$cy" 0,255,0 || rc=1
      probe uncovered_page "$file" "$px" "$py" 255,255,255 || rc=1
      ;;
    rescaled)
      local lx ly
      if [ "$kind" = x11 ]; then
        lx=$ox ly=$oy
      else
        lx=$((ox / s)) ly=$((oy / s))
      fi
      probe rescaled_child "$file" $((lx + 20)) $((ly + 20)) 0,255,0 || rc=1
      probe rescaled_page "$file" $((lx - 15)) $((ly + 10)) 255,255,255 || rc=1
      ;;
    *) log "::error::no probes for $step"; rc=1 ;;
  esac
  return "$rc"
}

term_rc=na
term_shots=0
term_passed=0
run_term() {
  local kind=$1 dir=$2 row=$3
  shift 3
  local steps=("$@")
  mkdir -p "$dir"
  NO_AT_BRIDGE=1 NATIVE_ROW_DIR="$dir" timeout 240 cargo test "${pkg[@]}" --lib -- --ignored --exact "$row" --test-threads=1 > "$dir/row.txt" 2>&1 &
  local pid=$! step
  term_shots=0
  term_passed=0
  for step in "${steps[@]}"; do
    if ! wait_step "$dir/$step.json" "$pid"; then
      log "::error::$cell ${row##*::}: the row never reached $step"
      break
    fi
    term_shots=$((term_shots + 1))
    pixels "$kind" "$step" "$dir" && term_passed=$((term_passed + 1))
    : > "$dir/go-$step"
  done
  wait "$pid"
  term_rc=$?
  tail -n 12 "$dir/row.txt" >> "$out"
  log "$cell ${row##*::}: row exit $term_rc, steps $term_shots of ${#steps[@]}, pixel steps passed $term_passed of ${#steps[@]}"
}

origin() {
  local file=$1 box
  box=$(convert "$file" -fill black +opaque '#ffffff' -format '%@' info: 2> /dev/null || echo unreadable)
  log "  $cell white toplevel box $box"
  [[ "$box" =~ ^640x480\+([0-9]+)\+([0-9]+)$ ]] || return 1
  ox=${BASH_REMATCH[1]}
  oy=${BASH_REMATCH[2]}
}

clip_pixels() {
  local kind=$1 step=$2 dir=$3 mode=$4
  local file="$dir/$step.png" ox oy rc=0
  shot "$kind" "$file" || { log "::error::$cell clip $step: no screenshot"; return 1; }
  origin "$file" || { log "::error::$cell clip $step: no 640x480 white toplevel in the screenshot"; return 1; }
  at() { probe "$1" "$file" $((ox + $2)) $((oy + $3)) "$4" || rc=1; }
  case "$mode:$step" in
    green:cropped | green:back)
      at "${step}_part" 350 250 0,255,0
      at "${step}_beside_part" 500 250 255,255,255
      at "${step}_below_part" 350 350 255,255,255
      at "${step}_outside_toplevel" 700 400 0,0,255
      at "${step}_outside_toplevel_low" 850 550 0,0,255
      ;;
    green:reclipped)
      at reclipped_part 330 230 0,255,0
      at reclipped_old_part 380 250 255,255,255
      at reclipped_outside_toplevel 700 400 0,0,255
      ;;
    green:none)
      at none_old_part 330 230 255,255,255
      at none_page 100 100 255,255,255
      at none_outside_toplevel 700 400 0,0,255
      ;;
    green:larger_old | green:larger)
      if [ "$step" = larger ] || [ "$kind" != x11 ]; then
        at "${step}_part" 260 160 0,255,0
        at "${step}_part_end" 390 290 0,255,0
      fi
      at "${step}_beside_part" 410 250 255,255,255
      at "${step}_below_part" 260 310 255,255,255
      at "${step}_outside_toplevel" 700 400 0,0,255
      at "${step}_outside_toplevel_low" 900 600 0,0,255
      ;;
    green:smaller_old | green:smaller | green:rescaled_old | green:rescaled)
      if [ "$step" != smaller_old ] || [ "$kind" != x11 ]; then
        at "${step}_part" 330 230 0,255,0
        at "${step}_part_end" 355 245 0,255,0
      fi
      at "${step}_beside_part" 370 230 255,255,255
      at "${step}_below_part" 330 260 255,255,255
      at "${step}_old_part" 260 160 255,255,255
      at "${step}_outside_toplevel" 700 400 0,0,255
      ;;
    *) log "::error::no probes for $mode $step"; rc=1 ;;
  esac
  return "$rc"
}

clip_rc=na
clip_reached=0
clip_passed=0
run_clip() {
  local kind=$1 dir=$2 mode=$3
  shift 3
  local steps=("$@")
  mkdir -p "$dir"
  NO_AT_BRIDGE=1 NATIVE_ROW_DIR="$dir" timeout 240 cargo test "${pkg[@]}" --lib -- --ignored --exact "$clip_row" --test-threads=1 > "$dir/row.txt" 2>&1 &
  local pid=$! step
  clip_reached=0
  clip_passed=0
  for step in "${steps[@]}"; do
    if ! wait_step "$dir/$step.json" "$pid"; then
      log "::error::$cell clip: the row never reached $step"
      break
    fi
    clip_reached=$((clip_reached + 1))
    if clip_pixels "$kind" "$step" "$dir" "$mode"; then
      clip_passed=$((clip_passed + 1))
    fi
    : > "$dir/go-$step"
  done
  wait "$pid"
  clip_rc=$?
  tail -n 25 "$dir/row.txt" >> "$out"
  log "$cell clip ($mode): row exit $clip_rc, steps $clip_reached of ${#steps[@]}, pixel steps passed $clip_passed of ${#steps[@]}"
}

wait_step() {
  local file=$1 pid=$2
  for _ in $(seq 1 1200); do
    [ -f "$file" ] && return 0
    kill -0 "$pid" 2> /dev/null || return 1
    sleep 0.1
  done
  return 1
}

cells=()
cell_rows=()
row_ok=0
run_cell() {
  cell=$1
  local kind=$2 load=$3
  local dir="$root/$cell"
  mkdir -p "$dir"
  log "== cell $cell ($kind, $load)"
  if ! "start_$kind"; then
    log "::error::$cell: the $kind display did not start"
    cells+=("$cell")
    cell_rows+=("| $cell | display did not start | | | | | | | | | | | |")
    row_ok=1
    stop_display
    return
  fi
  [ "$load" = loaded ] && load_on
  NO_AT_BRIDGE=1 NATIVE_ROW_DIR="$dir" timeout 240 cargo test "${pkg[@]}" --lib -- --ignored --exact "$display_row" --test-threads=1 > "$dir/row.txt" 2>&1 &
  local pid=$! step shots=0 probes_ok=0
  for step in placed hidden shown; do
    if ! wait_step "$dir/$step.json" "$pid"; then
      log "::error::$cell: the row never reached $step"
      break
    fi
    if pixels "$kind" "$step" "$dir"; then
      probes_ok=$((probes_ok + 1))
    fi
    shots=$((shots + 1))
    : > "$dir/go-$step"
  done
  wait "$pid"
  local rc=$?
  local clip_steps=(cropped reclipped none back larger_old larger smaller_old smaller)
  [ "$kind" = x11 ] || clip_steps+=(rescaled_old rescaled)
  run_clip "$kind" "$dir/clip" green "${clip_steps[@]}"
  NO_AT_BRIDGE=1 timeout 120 cargo test "${pkg[@]}" --lib -- --ignored --exact "$timeout_row" --test-threads=1 > "$dir/timeout.txt" 2>&1
  local timeout_rc=$?
  tail -n 8 "$dir/timeout.txt" >> "$out"
  run_term "$kind" "$dir/cover" "$cover_row" shown covered uncovered
  local cover_rc=$term_rc cover_passed=$term_passed
  run_term "$kind" "$dir/rescale" "$rescale_row" rescaled
  local rescale_rc=$term_rc rescale_passed=$term_passed
  local focus_rc=na
  if [ "$kind" != weston ]; then
    NO_AT_BRIDGE=1 timeout 120 cargo test "${pkg[@]}" --lib -- --ignored --exact "$focus_row" --test-threads=1 > "$dir/focus.txt" 2>&1
    focus_rc=$?
    tail -n 8 "$dir/focus.txt" >> "$out"
  fi
  load_off
  stop_display
  tail -n 25 "$dir/row.txt" >> "$out"
  local report=none
  [ -f "$dir/report.json" ] && report=$(jq -c '{present_ms, wait_ms, shown_wait_ms, shown_present_ms}' "$dir/report.json")
  log "$cell: row exit $rc, steps $shots of 3, pixel steps passed $probes_ok of 3, timeout row exit $timeout_rc, $report"
  local present wait
  present=$(jq -r '.present_ms | "\(.p50*100|round/100) / \(.p95*100|round/100) / \(.max*100|round/100)"' "$dir/report.json" 2> /dev/null || echo none)
  wait=$(jq -r '.wait_ms | "\(.p50*100|round/100) / \(.p95*100|round/100) / \(.max*100|round/100)"' "$dir/report.json" 2> /dev/null || echo none)
  cell_rows+=("| $cell | $rc | $probes_ok of 3 | $present | $wait | $clip_rc | $clip_passed of ${#clip_steps[@]} | $timeout_rc | $cover_rc | $cover_passed of 3 | $rescale_rc | $rescale_passed of 1 | $focus_rc |")
  if [ "$rc" != 0 ] || [ "$probes_ok" != 3 ] || [ "$clip_rc" != 0 ] || [ "$clip_passed" != "${#clip_steps[@]}" ] \
    || [ "$timeout_rc" != 0 ] || ! grep -qE '^test result: ok\. 1 passed;' "$dir/timeout.txt" \
    || [ "$cover_rc" != 0 ] || [ "$cover_passed" != 3 ] || [ "$rescale_rc" != 0 ] || [ "$rescale_passed" != 1 ] \
    || { [ "$kind" != weston ] && { [ "$focus_rc" != 0 ] || ! grep -qE '^test result: ok\. 1 passed;' "$dir/focus.txt"; }; }; then
    row_ok=1
    cells+=("$cell")
  fi
}

red_ok=1
red_rows_out=()
red_hit() {
  local file=$1 row=$2 message=$3 rc=$4
  [ "$rc" = 101 ] && grep -qxF "test $row ... FAILED" "$file" && grep -q "panicked at" "$file" && grep -qF "$message" "$file"
}
git checkout -q --detach "$L5C_RED_SHA"
log "== red $L5C_RED_SHA: the term stub; each term row must fail by its own assertion on X11, weston and sway (focus on X11 and sway)"
cargo test "${pkg[@]}" --lib --no-run > "$RUNNER_TEMP/red-build.txt" 2>&1
red_built=$?
tail -n 20 "$RUNNER_TEMP/red-build.txt" >> "$out"
if [ "$red_built" = 0 ]; then
  red_ok=0
  for red_kind in x11 weston sway; do
    cell="red-$red_kind"
    dir="$root/$cell"
    mkdir -p "$dir"
    log "== cell $cell ($red_kind, quiet, red $L5C_RED_SHA)"
    if ! "start_$red_kind"; then
      log "::error::$cell: the $red_kind display did not start"
      red_ok=1
      red_rows_out+=("| $cell | display did not start | | | | | |")
      stop_display
      continue
    fi
    NO_AT_BRIDGE=1 timeout 120 cargo test "${pkg[@]}" --lib -- --ignored --exact "$cover_row" --test-threads=1 > "$dir/cover.txt" 2>&1
    cover_rc=$?
    NO_AT_BRIDGE=1 timeout 120 cargo test "${pkg[@]}" --lib -- --ignored --exact "$rescale_row" --test-threads=1 > "$dir/rescale.txt" 2>&1
    rescale_rc=$?
    focus_rc=na
    focus_hit=na
    if [ "$red_kind" != weston ]; then
      NO_AT_BRIDGE=1 timeout 120 cargo test "${pkg[@]}" --lib -- --ignored --exact "$focus_row" --test-threads=1 > "$dir/focus.txt" 2>&1
      focus_rc=$?
      focus_hit=1
      red_hit "$dir/focus.txt" "$focus_row" "$focus_message" "$focus_rc" && focus_hit=0
      tail -n 12 "$dir/focus.txt" >> "$out"
    fi
    stop_display
    cover_hit=1
    red_hit "$dir/cover.txt" "$cover_row" "$cover_message" "$cover_rc" && cover_hit=0
    rescale_hit=1
    red_hit "$dir/rescale.txt" "$rescale_row" "$rescale_message" "$rescale_rc" && rescale_hit=0
    tail -n 12 "$dir/cover.txt" "$dir/rescale.txt" >> "$out"
    red_rows_out+=("| $cell | $cover_rc | $cover_hit | $rescale_rc | $rescale_hit | $focus_rc | $focus_hit |")
    if [ "$cover_hit" != 0 ] || [ "$rescale_hit" != 0 ] || { [ "$red_kind" != weston ] && [ "$focus_hit" != 0 ]; }; then
      red_ok=1
    fi
  done
fi
git checkout -q -f --detach "$GREEN_SHA"
[ -z "$(git status --porcelain --untracked-files=no)" ] || { log "::error::the tree is not clean at the green tip"; rows_built=1; }
log "== green: lib rows build again after the red cells"
cargo test "${pkg[@]}" --lib --no-run >> "$out" 2>&1 || rows_built=1

if [ "$rows_built" = 0 ]; then
  run_cell x11 x11 quiet
  run_cell weston weston quiet
  run_cell sway sway quiet
  run_cell x11-loaded x11 loaded
  run_cell weston-loaded weston loaded
  run_cell sway-loaded sway loaded
else
  row_ok=1
fi

log "== green: clippy --lib"
cargo clippy "${pkg[@]}" --lib -- -D warnings 2>&1 | tee -a "$out"
clip=${PIPESTATUS[0]}
log "== green: clippy --tests"
cargo clippy "${pkg[@]}" --tests -- -D warnings 2>&1 | tee -a "$out"
clipt=${PIPESTATUS[0]}

{
  printf '| %s | exit |\n| --- | --- |\n' "$os"
  printf '| base %s, red %s and green %s in line (0 = yes) | %s |\n' "$BASE_SHA" "$L5C_RED_SHA" "$GREEN_SHA" "$anc"
  printf '| red %s: lib rows build | %s |\n' "$L5C_RED_SHA" "$red_built"
  printf '| red %s: every term row fails by its own assertion in every red cell (0 = yes) | %s |\n' "$L5C_RED_SHA" "$red_ok"
  printf '| green lib build | %s |\n| green lib rows build | %s |\n' "$lib_built" "$rows_built"
  printf '| native_term rows | %s, %s passed |\n' "$rows_rc" "$passed"
  printf '| named rows ok, none failed (0 = yes) | %s |\n' "$want"
  printf '| display row cells all pass (0 = yes) | %s %s |\n' "$row_ok" "${cells[*]:-}"
  printf '| clippy --lib -D warnings | %s |\n| clippy --tests -D warnings | %s |\n' "$clip" "$clipt"
  printf '\n| cell | row exit | pixel steps | present ms p50 / p95 / max | wait for ready ms p50 / p95 / max | clip row exit | clip pixel steps | timeout row exit | cover row exit | cover pixel steps | rescale row exit | rescale pixel steps | focus row exit (na on weston) |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n'
  printf '%s\n' "${cell_rows[@]}"
  printf '\n| red cell | cover row exit (want 101) | cover fails by its assertion (0 = yes) | rescale row exit (want 101) | rescale fails by its assertion (0 = yes) | focus row exit (want 101, na on weston) | focus fails by its assertion (0 = yes) |\n| --- | --- | --- | --- | --- | --- | --- |\n'
  printf '%s\n' "${red_rows_out[@]}"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$anc" = 0 ] && [ "$red_built" = 0 ] && [ "$red_ok" = 0 ] && [ "$lib_built" = 0 ] && [ "$rows_built" = 0 ] && [ "$want" = 0 ] \
  && [ "$rows_rc" = 0 ] && [ "$row_ok" = 0 ] && [ "$clip" = 0 ] && [ "$clipt" = 0 ]
