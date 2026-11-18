#!/usr/bin/env bash
set +e -uo pipefail
if [ -z "${FARM_IN_SESSION:-}" ]; then
  exec dbus-run-session -- env FARM_IN_SESSION=1 bash "$0" "$@"
fi

os="$1"
case "$os" in
  linux) ;;
  *) echo "::error::the L4 gate rows run on Linux only, not $os"; exit 2 ;;
esac
out="$RUNNER_TEMP/v244-linux-gates-$os.txt"
: > "$out"
root="$RUNNER_TEMP/gates-cells"
mkdir -p "$root"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }

clip_row=native_term::linux::clip::tests::the_desktop_boards_take_and_give_text_only_on_user_input
middle_row=native_term::linux::view::tests::a_middle_click_pastes_primary_through_the_paste_path
pointer_row=native_term::linux::view::tests::a_covered_view_leaves_clicks_and_wheel_to_the_page_overlay
shown_row=native_term::linux::view::tests::a_view_off_screen_refuses_native_focus_and_going_off_screen_drops_it
press_row=native_term::linux::view::tests::a_press_while_another_button_is_down_goes_to_the_page
unplaced_row=native_term::linux::view::tests::an_unplaced_view_sends_its_keys_on_to_the_page
clipped_row=native_term::linux::view::tests::a_view_clipped_out_of_the_page_sends_its_keys_on_to_the_page
superseded_row=native_term::linux::view::tests::a_press_in_one_view_takes_native_focus_from_the_other
guard_row=native_term::linux::clip::tests::only_a_key_or_button_event_reads_or_writes_a_board
bound_row=native_term::linux::clip::tests::a_read_longer_than_a_copy_is_dropped
cover_row=native_term::linux::view::tests::a_primary_paste_lands_only_on_a_placed_uncovered_view
flatten_row=native_term::pane::tests::a_primary_paste_flattens_its_line_breaks_unless_the_app_brackets
timeout_row=native_term::linux::clip::tests::a_board_call_from_a_timeout_is_refused
red_rows=("$guard_row" "$bound_row" "$cover_row" "$flatten_row")
kept_rows=(
  "$shown_row"
  "$press_row"
  native_term::linux::clip::tests::a_private_board_keeps_each_board_in_process
  native_term::linux::view::tests::the_right_button_opens_the_menu_and_other_buttons_are_held_without_events
  native_term::linux::view::tests::a_press_inside_the_uncovered_rect_takes_focus_and_counts_clicks
  native_term::linux::view::tests::a_press_outside_the_rect_on_a_covered_view_or_before_placing_goes_to_the_page
  native_term::linux::view::tests::a_left_drag_follows_the_pointer_outside_the_rect_until_release
  native_term::linux::view::tests::a_cover_during_a_press_leaves_the_rest_of_it_to_the_page
  native_term::linux::view::tests::a_wheel_inside_is_lines_for_a_mouse_and_points_for_a_touchpad
  native_term::linux::view::tests::clicks_reset_after_the_double_click_time_or_distance
  native_term::linux::view::tests::the_terminal_chords_copy_paste_and_select_all
  native_term::linux::view::tests::a_chord_on_a_non_latin_layout_uses_the_keys_latin_group
  native_term::linux::view::tests::a_commit_lands_only_while_the_view_is_active
  native_term::linux::view::tests::keys_go_to_the_view_only_with_native_and_toplevel_focus_and_no_cover
  native_term::linux::view::tests::a_cover_drops_native_focus_until_the_page_gives_it_back
  native_term::linux::view::tests::a_middle_press_inside_reads_primary_and_is_held_until_release
  native_term::linux::keys::tests::control_on_a_non_latin_layout_uses_the_keys_latin_group
  native_term::linux::surface::tests::a_child_presents_only_with_a_change_and_no_frame_callback_owed
  native_term::keymap::tests::named_keys_carry_their_modifiers
  native_term::render::tests::each_platform_draws_with_its_own_backends
)
ignored_rows=(
  "$clip_row|needs a display and the farm's clipboard tools: the farm runs it under Xvfb and sway"
  "$middle_row|needs a display, the farm's pointer and its clipboard tools: the farm runs it under Xvfb and sway"
  "$pointer_row|needs a display and the farm's pointer: the farm runs it under Xvfb and sway"
  "native_term::linux::view::tests::a_focused_view_takes_keys_and_commits_after_the_page_had_them|needs a display, IBus and the farm's typing: the farm runs it under Xvfb and sway"
  "$unplaced_row|needs a display: the farm runs it under Xvfb and sway"
  "$clipped_row|needs a display: the farm runs it under Xvfb and sway"
  "$superseded_row|needs a display: the farm runs it under Xvfb and sway"
  "$timeout_row|needs a display: the farm runs it under Xvfb and sway"
)

mkdir -p apps/desktop/dist
[ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
log "os $os red $RED1_SHA green $GREEN_SHA $(rustc -V)"
log "$(sway --version 2>&1 | head -1); xclip $(command -v xclip > /dev/null && echo present || echo missing); wl-copy $(command -v wl-copy > /dev/null && echo present || echo missing); xkbcli $(command -v xkbcli > /dev/null && echo present || echo missing); xdotool $(xdotool version 2>&1 | head -1)"
xkbcli compile-keymap --layout us > "$RUNNER_TEMP/keymap-us.xkb" 2> "$RUNNER_TEMP/keymap-us.err" || log "::error::xkbcli did not compile the us keymap: $(head -c 300 "$RUNNER_TEMP/keymap-us.err")"

red_ok=0
red_table=()
red_build() {
  local sha=$1
  git checkout -q --detach "$sha"
  log "== red $sha: build the lib rows"
  cargo test "${pkg[@]}" --lib --no-run > "$RUNNER_TEMP/red-build-$sha.txt" 2>&1
  local built=$?
  tail -n 20 "$RUNNER_TEMP/red-build-$sha.txt" >> "$out"
  red_table+=("| red $sha: lib rows build | $built |")
  [ "$built" = 0 ] || red_ok=1
}
red_fails() {
  local sha=$1 row=$2 how=$3
  local file="$RUNNER_TEMP/red-run-${row##*::}.txt" rc failed=1
  log "== red $sha: $row must fail by its assertion"
  if [ "$how" = x11 ]; then
    DISPLAY=:98 GDK_BACKEND=x11 NO_AT_BRIDGE=1 timeout 180 cargo test "${pkg[@]}" --lib -- --ignored --exact "$row" --test-threads=1 > "$file" 2>&1
  else
    cargo test "${pkg[@]}" --lib -- --exact "$row" > "$file" 2>&1
  fi
  rc=$?
  tail -n 20 "$file" >> "$out"
  if [ "$rc" = 101 ] && grep -qxF "test $row ... FAILED" "$file" && grep -q "panicked at" "$file"; then
    failed=0
  fi
  [ "$failed" = 0 ] || red_ok=1
  red_table+=("| red $sha: ${row##*::} fails by its assertion ($how; 0 = yes) | $failed |")
  log "red $sha: $row exit $rc, failed by its assertion (0 = yes) $failed"
}
Xvfb :98 -screen 0 1280x800x24 -nolisten tcp -noreset > "$RUNNER_TEMP/xvfb-red.log" 2>&1 &
red_xvfb=$!
for _ in $(seq 1 50); do
  [ -S /tmp/.X11-unix/X98 ] && break
  sleep 0.1
done
[ -S /tmp/.X11-unix/X98 ] || { log "::error::the red Xvfb did not start"; red_ok=1; }
red_build "$RED1_SHA"
red_fails "$RED1_SHA" "$guard_row" plain
red_fails "$RED1_SHA" "$bound_row" plain
red_fails "$RED1_SHA" "$cover_row" plain
red_fails "$RED1_SHA" "$flatten_row" plain
red_fails "$RED1_SHA" "$timeout_row" x11
kill "$red_xvfb" 2> /dev/null
wait "$red_xvfb" 2> /dev/null
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
  for pair in "${ignored_rows[@]}"; do
    grep -qxF "test ${pair%%|*} ... ignored, ${pair#*|}" "$RUNNER_TEMP/rows.txt" \
      || { log "::error::${pair%%|*} is not ignored in a plain run"; want=1; }
  done
  grep -E '^test result:' "$RUNNER_TEMP/rows.txt" | tee -a "$out"
fi

export WEBKIT_DISABLE_DMABUF_RENDERER=1 NO_AT_BRIDGE=1
unset DISPLAY WAYLAND_DISPLAY GTK_IM_MODULE XMODIFIERS

display_pid=""
input_pid=""
input_fd=""
input_sent=0
input_acks=""
load_pids=()

sway_keyboards() {
  local sock
  sock=$(ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock 2> /dev/null | head -1)
  [ -n "$sock" ] || { echo "no sway ipc socket"; return; }
  SWAYSOCK=$sock timeout 5 swaymsg -t get_inputs -r 2> /dev/null | jq -r '[.[] | select(.type == "keyboard")] | length'
}
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

start_seat() {
  local fifo="$root/seat-$cell.fifo"
  input_acks="$root/seat-$cell.acks"
  rm -f "$fifo" "$input_acks"
  mkfifo "$fifo"
  : > "$input_acks"
  python3 -I "$RUNNER_TEMP/wl-input.py" "$fifo" "$input_acks" "$RUNNER_TEMP/keymap-us.xkb" 1280x800 > "$root/seat-$cell.log" 2>&1 &
  input_pid=$!
  exec {input_fd}<> "$fifo"
  input_sent=0
  for _ in $(seq 1 50); do
    grep -qx ready "$input_acks" && return 0
    kill -0 "$input_pid" 2> /dev/null || break
    sleep 0.1
  done
  log "::error::$cell: the farm seat did not start: $(tail -n 3 "$root/seat-$cell.log" | tr '\n' ' ')"
  return 1
}

seat_do() {
  local want=$((input_sent + 1))
  input_sent=$want
  printf '%s\n' "$*" >&"$input_fd"
  for _ in $(seq 1 300); do
    [ "$(grep -c '^done ' "$input_acks")" -ge "$want" ] && return 0
    kill -0 "$input_pid" 2> /dev/null || break
    sleep 0.05
  done
  log "::error::$cell: the farm seat did not run $*: $(tail -n 3 "$root/seat-$cell.log" | tr '\n' ' ')"
  return 1
}

stop_seat() {
  [ -n "$input_fd" ] && exec {input_fd}>&-
  input_fd=""
  [ -n "$input_pid" ] && kill "$input_pid" 2> /dev/null
  [ -n "$input_pid" ] && wait "$input_pid" 2> /dev/null
  input_pid=""
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
      start_seat || return 1
      log "  $cell sway keyboards with the farm seat: $(sway_keyboards)"
      return 0
    fi
    sleep 0.1
  done
  return 1
}

stop_display() {
  stop_seat
  pkill -x xclip 2> /dev/null
  pkill -x wl-copy 2> /dev/null
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

focus_window() {
  local kind=$1
  [ "$kind" = x11 ] || return 0
  timeout 10 xdotool search --sync --limit 1 --name '^native-row$' windowfocus --sync > /dev/null 2>&1
  log "  $cell window focus exit $?"
}

press_key() {
  local kind=$1 name=$2 rc
  if [ "$kind" = x11 ]; then
    timeout 30 xdotool key --delay 80 "$name"
    rc=$?
  else
    seat_do type "$name"
    rc=$?
  fi
  log "  $cell key $name exit $rc"
}

board_read() {
  local kind=$1 board=$2 file=$3 rc
  if [ "$kind" = x11 ]; then
    timeout 10 xclip -o -selection "$board" > "$file" 2> "$file.err"
    rc=$?
  elif [ "$board" = primary ]; then
    timeout 10 wl-paste -n -p > "$file" 2> "$file.err"
    rc=$?
  else
    timeout 10 wl-paste -n > "$file" 2> "$file.err"
    rc=$?
  fi
  log "  $cell farm read $board exit $rc: $(head -c 80 "$file") $(head -c 120 "$file.err")"
}

board_write() {
  local kind=$1 board=$2 text=$3 rc
  if [ "$kind" = x11 ]; then
    printf '%s' "$text" | timeout 10 xclip -i -selection "$board"
    rc=$?
  elif [ "$board" = primary ]; then
    printf '%s' "$text" | timeout 10 wl-copy -p
    rc=$?
  else
    printf '%s' "$text" | timeout 10 wl-copy
    rc=$?
  fi
  sleep 0.5
  log "  $cell farm write $board $text exit $rc"
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

point_click() {
  local kind=$1 x=$2 y=$3 button=$4 rc
  if [ "$kind" = x11 ]; then
    local number=1
    [ "$button" = middle ] && number=2
    timeout 10 xdotool mousemove "$x" "$y" sleep 0.2 click "$number"
    rc=$?
  else
    seat_do click "$x" "$y" "$button"
    rc=$?
  fi
  log "  $cell $button click ($x,$y) exit $rc"
}

point_wheel() {
  local kind=$1 rc
  if [ "$kind" = x11 ]; then
    timeout 10 xdotool click 5
    rc=$?
  else
    seat_do scroll 1
    rc=$?
  fi
  log "  $cell wheel down exit $rc"
}

act_clip() {
  local kind=$1 step=$2 dir=$3
  case "$step" in
    write)
      focus_window "$kind"
      press_key "$kind" w
      ;;
    written)
      board_read "$kind" clipboard "$dir/farm-clipboard.txt"
      board_read "$kind" primary "$dir/farm-primary.txt"
      ;;
    refused) board_read "$kind" clipboard "$dir/farm-clipboard.txt" ;;
    read)
      board_write "$kind" clipboard farm-clip-2
      board_write "$kind" primary farm-primary-2
      focus_window "$kind"
      press_key "$kind" r
      ;;
  esac
}

act_pointer() {
  local kind=$1 step=$2 dir=$3 cx cy ox oy
  read -r cx cy ox oy < <(jq -r '[.click[0], .click[1], .outside[0], .outside[1]] | @tsv' "$dir/$step.json")
  case "$step" in
    covered)
      focus_window "$kind"
      point_click "$kind" "$cx" "$cy" left
      point_wheel "$kind"
      ;;
    uncovered)
      point_click "$kind" "$cx" "$cy" left
      point_wheel "$kind"
      ;;
    outside) point_click "$kind" "$ox" "$oy" left ;;
  esac
}

act_middle() {
  local kind=$1 step=$2 dir=$3 cx cy
  read -r cx cy < <(jq -r '[.click[0], .click[1]] | @tsv' "$dir/$step.json")
  case "$step" in
    primary)
      board_write "$kind" primary farm-primary-3
      focus_window "$kind"
      point_click "$kind" "$cx" "$cy" middle
      ;;
    covered-primary) point_click "$kind" "$cx" "$cy" middle ;;
  esac
}

cells=()
cell_rows=()
row_ok=0
run_row() {
  local kind=$1 dir=$2 name=$3 row=$4
  shift 4
  local steps=("$@")
  mkdir -p "$dir"
  NATIVE_ROW_DIR="$dir" timeout 300 cargo test "${pkg[@]}" --lib -- --ignored --exact "$row" --test-threads=1 > "$dir/row.txt" 2>&1 &
  local pid=$! step reached=0 last=none
  for step in "${steps[@]}"; do
    if ! wait_step "$dir/$step.json" "$pid"; then
      log "::error::$cell $name: the row never reached $step"
      break
    fi
    reached=$((reached + 1))
    last=$step
    [ "$kind" = sway ] && log "  $cell at $step sway keyboards: $(sway_keyboards)"
    "act_$name" "$kind" "$step" "$dir"
    : > "$dir/go-$step"
  done
  wait "$pid"
  local rc=$?
  tail -n 30 "$dir/row.txt" >> "$out"
  local panic
  panic=$(grep -m1 -E "panicked at|assertion" "$dir/row.txt" | cut -c1-160)
  log "$cell $name: row exit $rc, steps reached $reached of ${#steps[@]} (last $last)"
  cell_rows+=("| $cell | $name | $rc | $reached of ${#steps[@]} ($last) | ${panic:-} |")
  if [ "$rc" != 0 ] || [ "$reached" != "${#steps[@]}" ]; then
    row_ok=1
    cells+=("$cell-$name")
  fi
}

run_cell() {
  cell=$1
  local kind=$2 load=$3
  local dir="$root/$cell"
  mkdir -p "$dir"
  log "== cell $cell ($kind, $load)"
  if ! "start_$kind"; then
    log "::error::$cell: the $kind display did not start"
    cells+=("$cell")
    cell_rows+=("| $cell | all | display did not start | | |")
    row_ok=1
    stop_display
    return
  fi
  [ "$load" = loaded ] && load_on
  run_row "$kind" "$dir/clip" clip "$clip_row" write written refused read
  run_row "$kind" "$dir/middle" middle "$middle_row" primary covered-primary
  run_row "$kind" "$dir/pointer" pointer "$pointer_row" covered uncovered outside
  run_row "$kind" "$dir/unplaced" unplaced "$unplaced_row"
  run_row "$kind" "$dir/clipped" clipped "$clipped_row"
  run_row "$kind" "$dir/superseded" superseded "$superseded_row"
  run_row "$kind" "$dir/timeout" timeout "$timeout_row"
  load_off
  stop_display
}

if [ "$rows_built" = 0 ]; then
  run_cell x11 x11 quiet
  run_cell sway sway quiet
  run_cell x11-loaded x11 loaded
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
  printf '%s\n' "${red_table[@]}"
  printf '| green lib build | %s |\n| green lib rows build | %s |\n' "$lib_built" "$rows_built"
  printf '| native_term rows | %s, %s passed |\n' "$rows_rc" "$passed"
  printf '| named rows ok, none failed, display rows ignored (0 = yes) | %s |\n' "$want"
  printf '| display row cells all pass (0 = yes) | %s %s |\n' "$row_ok" "${cells[*]:-}"
  printf '| sway input | the farm seat: one virtual keyboard and one virtual pointer for each cell |\n'
  printf '| clippy --lib -D warnings | %s |\n| clippy --tests -D warnings | %s |\n' "$clip" "$clipt"
  printf '\n| cell | row | row exit | steps reached | first panic |\n| --- | --- | --- | --- | --- |\n'
  printf '%s\n' "${cell_rows[@]}"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$red_ok" = 0 ] && [ "$lib_built" = 0 ] && [ "$rows_built" = 0 ] && [ "$want" = 0 ] \
  && [ "$rows_rc" = 0 ] && [ "$row_ok" = 0 ] && [ "$clip" = 0 ] && [ "$clipt" = 0 ]
