#!/usr/bin/env bash
set +e -uo pipefail

os="$1"
case "$os" in
  linux) ;;
  *) echo "::error::the surface clip rows run on Linux only, not $os"; exit 2 ;;
esac
out="$RUNNER_TEMP/v244-linux-l5a-$os.txt"
: > "$out"
root="$RUNNER_TEMP/l5a-cells"
mkdir -p "$root"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }

clip_row=native_term::linux::surface::tests::a_child_shows_only_its_part_inside_the_webview_and_reclips_on_size_allocate
timeout_row=native_term::linux::clip::tests::a_board_call_from_a_timeout_is_refused
rescale_message="a present at the old size and scale shows the whole part after a rescale"
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
  native_term::keymap::tests::named_keys_carry_their_modifiers
  native_term::render::tests::each_platform_draws_with_its_own_backends
  native_term::style::tests::on_linux_monospace_is_dejavu_sans_mono_else_the_built_in_font
)

mkdir -p apps/desktop/dist
[ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
log "os $os rescale red $RESCALE_RED_SHA green $GREEN_SHA $(rustc -V)"
log "weston $(weston --version 2>&1 | head -1); $(sway --version 2>&1 | head -1); $(vulkaninfo --summary 2>/dev/null | grep -m1 deviceName)"
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
  grep -qx "test $timeout_row ... ignored, needs a display: the farm runs it under Xvfb and sway" "$RUNNER_TEMP/rows.txt" \
    || { log "::error::$timeout_row is not ignored in a plain run"; want=1; }
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
    hidden)
      probe hidden_child_area "$file" "$cx" "$cy" 255,255,255 || rc=1
      ;;
  esac
  return "$rc"
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
    cell_rows+=("| $cell | display did not start | | | | | |")
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
  load_off
  stop_display
  tail -n 25 "$dir/row.txt" >> "$out"
  local report=none
  [ -f "$dir/report.json" ] && report=$(jq -c '{present_ms, wait_ms, shown_wait_ms, shown_present_ms}' "$dir/report.json")
  log "$cell: row exit $rc, steps $shots of 3, pixel steps passed $probes_ok of 3, timeout row exit $timeout_rc, $report"
  local present wait
  present=$(jq -r '.present_ms | "\(.p50*100|round/100) / \(.p95*100|round/100) / \(.max*100|round/100)"' "$dir/report.json" 2> /dev/null || echo none)
  wait=$(jq -r '.wait_ms | "\(.p50*100|round/100) / \(.p95*100|round/100) / \(.max*100|round/100)"' "$dir/report.json" 2> /dev/null || echo none)
  cell_rows+=("| $cell | $rc | $probes_ok of 3 | $present | $wait | $clip_rc | $clip_passed of ${#clip_steps[@]} | $timeout_rc |")
  if [ "$rc" != 0 ] || [ "$probes_ok" != 3 ] || [ "$clip_rc" != 0 ] || [ "$clip_passed" != "${#clip_steps[@]}" ] \
    || [ "$timeout_rc" != 0 ] || ! grep -qE '^test result: ok\. 1 passed;' "$dir/timeout.txt"; then
    row_ok=1
    cells+=("$cell")
  fi
}

red_ok=1
red_rows_out=()
git checkout -q --detach "$RESCALE_RED_SHA"
log "== red $RESCALE_RED_SHA: the clip row's rescale step, where the label ignores the configured scale, on weston and sway (X11 has no buffer scale)"
cargo test "${pkg[@]}" --lib --no-run > "$RUNNER_TEMP/red-build.txt" 2>&1
red_built=$?
tail -n 20 "$RUNNER_TEMP/red-build.txt" >> "$out"
if [ "$red_built" = 0 ]; then
  red_ok=0
  for red_kind in weston sway; do
    cell="red-$red_kind"
    mkdir -p "$root/$cell"
    log "== cell $cell ($red_kind, quiet, red $RESCALE_RED_SHA)"
    if ! "start_$red_kind"; then
      log "::error::$cell: the $red_kind display did not start"
      red_ok=1
      red_rows_out+=("| $cell | display did not start | | |")
      stop_display
      continue
    fi
    run_clip "$red_kind" "$root/$cell/clip" green cropped reclipped none back larger_old larger smaller_old smaller rescaled_old
    stop_display
    red_message=1
    grep -qF "$rescale_message" "$root/$cell/clip/row.txt" && red_message=0
    red_rows_out+=("| $cell | $clip_rc | $clip_reached of 9 | $clip_passed of 9 | $red_message |")
    if [ "$clip_rc" != 101 ] || [ "$clip_reached" != 9 ] || [ "$red_message" != 0 ] \
      || ! grep -q "panicked at" "$root/$cell/clip/row.txt"; then
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

mutate=apps/desktop/src-tauri/src/native_term/linux/clip.rs
guard='        if !user_input(current_input()) {'
mutation_ok=1
mutation_rc=na
log "== mutation at $GREEN_SHA: the private read skips the seat guard; the seeded timeout row must fail by its assertion"
if [ "$(grep -cxF "$guard" "$mutate")" = 1 ]; then
  sed -i 's/^        if !user_input(current_input()) {$/        if false {/' "$mutate"
  git diff --stat | tee -a "$out"
  cell=mutation-x11
  mkdir -p "$root/$cell"
  if start_x11; then
    NO_AT_BRIDGE=1 timeout 900 cargo test "${pkg[@]}" --lib -- --ignored --exact "$timeout_row" --test-threads=1 > "$root/$cell/timeout.txt" 2>&1
    mutation_rc=$?
    tail -n 12 "$root/$cell/timeout.txt" >> "$out"
    if [ "$mutation_rc" = 101 ] && grep -qxF "test $timeout_row ... FAILED" "$root/$cell/timeout.txt" \
      && grep -q "panicked at" "$root/$cell/timeout.txt"; then
      mutation_ok=0
    fi
  else
    log "::error::$cell: the x11 display did not start"
  fi
  stop_display
else
  log "::error::the read guard line is not in $mutate exactly once"
fi
git checkout -q -f -- "$mutate"
[ -z "$(git status --porcelain --untracked-files=no)" ] || { log "::error::the mutation is still in the tree"; mutation_ok=1; }

label_ok=1
label_rc=na
label_file=apps/desktop/src-tauri/src/native_term/linux/surface.rs
label_message="a present at the new size carries the new scale"
log "== mutation at $GREEN_SHA: drawing labels every buffer 1; on weston the clip row must reach its rescaled step and fail by its label assertion"
label_at=$(grep -nxF '    fn drawing(&self, size: (u32, u32), scale: i32) {' "$label_file" | cut -d: -f1)
if [ -n "$label_at" ] && [ "$(sed -n "$((label_at + 1)),$((label_at + 8))p" "$label_file" | grep -cx '            scale,')" = 1 ]; then
  sed -i "$((label_at + 1)),$((label_at + 8))s/^            scale,\$/            scale: 1,/" "$label_file"
  git diff | tee -a "$out"
  cargo test "${pkg[@]}" --lib --no-run > "$RUNNER_TEMP/label-build.txt" 2>&1
  label_built=$?
  tail -n 5 "$RUNNER_TEMP/label-build.txt" >> "$out"
  cell=mutation-label-weston
  mkdir -p "$root/$cell"
  if [ "$label_built" = 0 ] && start_weston; then
    run_clip weston "$root/$cell/clip" green cropped reclipped none back larger_old larger smaller_old smaller rescaled_old rescaled
    label_rc=$clip_rc
    if [ "$clip_rc" = 101 ] && [ "$clip_reached" = 10 ] && grep -qF "$label_message" "$root/$cell/clip/row.txt" \
      && grep -q "panicked at" "$root/$cell/clip/row.txt"; then
      label_ok=0
    fi
  else
    log "::error::$cell: the label mutation did not build (exit $label_built) or weston did not start"
  fi
  stop_display
else
  log "::error::the label line is not in drawing exactly once"
fi
git checkout -q -f -- "$label_file"
[ -z "$(git status --porcelain --untracked-files=no)" ] || { log "::error::the label mutation is still in the tree"; label_ok=1; }

{
  printf '| %s | exit |\n| --- | --- |\n' "$os"
  printf '| red %s: lib rows build | %s |\n' "$RESCALE_RED_SHA" "$red_built"
  printf '| red %s: on weston and sway the clip row reaches its rescale step and fails by its assertion (0 = yes) | %s |\n' "$RESCALE_RED_SHA" "$red_ok"
  printf '| mutation at %s: a read that skips the seat guard fails the seeded timeout row by its assertion (0 = yes; row exit %s) | %s |\n' "$GREEN_SHA" "$mutation_rc" "$mutation_ok"
  printf '| mutation at %s: a drawing that labels every buffer 1 fails the clip row at its rescaled step by the label assertion, on weston (0 = yes; row exit %s) | %s |\n' "$GREEN_SHA" "$label_rc" "$label_ok"
  printf '| green lib build | %s |\n| green lib rows build | %s |\n' "$lib_built" "$rows_built"
  printf '| native_term rows | %s, %s passed |\n' "$rows_rc" "$passed"
  printf '| named rows ok, none failed (0 = yes) | %s |\n' "$want"
  printf '| display row cells all pass (0 = yes) | %s %s |\n' "$row_ok" "${cells[*]:-}"
  printf '| clippy --lib -D warnings | %s |\n| clippy --tests -D warnings | %s |\n' "$clip" "$clipt"
  printf '\n| cell | row exit | pixel steps | present ms p50 / p95 / max | wait for ready ms p50 / p95 / max | clip row exit | clip pixel steps | timeout row exit |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n'
  printf '%s\n' "${cell_rows[@]}"
  printf '\n| red cell | clip row exit (want 101) | steps reached (want 9) | pixel steps passed (rescaled_old fails under the red) | assertion message seen (0 = yes) |\n| --- | --- | --- | --- | --- |\n'
  printf '%s\n' "${red_rows_out[@]}"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$red_ok" = 0 ] && [ "$mutation_ok" = 0 ] && [ "$label_ok" = 0 ] && [ "$lib_built" = 0 ] && [ "$rows_built" = 0 ] && [ "$want" = 0 ] \
  && [ "$rows_rc" = 0 ] && [ "$row_ok" = 0 ] && [ "$clip" = 0 ] && [ "$clipt" = 0 ]
