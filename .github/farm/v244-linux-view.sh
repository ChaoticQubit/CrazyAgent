#!/usr/bin/env bash
set +e -uo pipefail
if [ -z "${FARM_IN_SESSION:-}" ]; then
  exec dbus-run-session -- env FARM_IN_SESSION=1 bash "$0" "$@"
fi

os="$1"
case "$os" in
  linux) ;;
  *) echo "::error::the L3a view rows run on Linux only, not $os"; exit 2 ;;
esac
out="$RUNNER_TEMP/v244-linux-view-$os.txt"
: > "$out"
root="$RUNNER_TEMP/view-cells"
mkdir -p "$root"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }

red_rows=(
  native_term::linux::view::tests::the_terminal_chords_copy_paste_and_select_all
  native_term::linux::view::tests::other_chords_stay_keys
  native_term::linux::view::tests::a_commit_is_one_text_input_within_its_bound
  native_term::linux::view::tests::keys_go_to_the_view_only_with_native_and_toplevel_focus_and_no_cover
  native_term::linux::view::tests::a_cover_drops_native_focus_until_the_page_gives_it_back
)
display_row=native_term::linux::view::tests::a_focused_view_takes_keys_and_commits_after_the_page_had_them
kept_rows=(
  native_term::linux::surface::tests::a_host_rect_is_finite_bounded_and_at_least_one_pixel
  native_term::linux::surface::tests::device_pixels_scale_the_logical_rect
  native_term::linux::surface::tests::a_scale_out_of_range_is_a_bug
  native_term::linux::surface::tests::a_child_presents_only_with_a_change_and_no_frame_callback_owed
  native_term::linux::surface::tests::a_hidden_child_never_presents_and_showing_redraws_it
  native_term::linux::surface::tests::a_present_that_did_not_happen_is_owed_again
  native_term::linux::surface::tests::a_child_whose_toplevel_unmapped_never_presents_again
  native_term::linux::surface::tests::presenting_when_not_ready_is_a_bug
  native_term::linux::keys::tests::letters_carry_control_and_alt_as_xterm_bytes
  native_term::linux::keys::tests::super_and_hyper_chords_send_nothing
  native_term::keymap::tests::named_keys_carry_their_modifiers
  native_term::render::tests::each_platform_draws_with_its_own_backends
)
ignored_rows=(
  "$display_row|needs a display, IBus and the farm's typing: the farm runs it under Xvfb and sway"
  "native_term::linux::surface::tests::a_child_places_hides_shows_and_presents_only_on_its_frame_callback|needs a display: the farm runs it under Xvfb, weston and sway"
)

mkdir -p apps/desktop/dist
[ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
log "os $os red $RED_SHA green $GREEN_SHA $(rustc -V)"
log "$(sway --version 2>&1 | head -1); $(ibus version 2>&1 | head -1); wtype $(command -v wtype > /dev/null && echo present || echo missing); xdotool $(xdotool version 2>&1 | head -1)"

git checkout -q --detach "$RED_SHA"
log "== red $RED_SHA: build the lib rows"
cargo test "${pkg[@]}" --lib --no-run > "$RUNNER_TEMP/red-build.txt" 2>&1
red_built=$?
tail -n 40 "$RUNNER_TEMP/red-build.txt" >> "$out"
grep -E '^error(\[E[0-9]+\])?:' "$RUNNER_TEMP/red-build.txt" | sort | uniq -c | head -20 | tee -a "$out"
: > "$RUNNER_TEMP/red-list.txt"
if [ "$red_built" = 0 ]; then
  cargo test "${pkg[@]}" --lib -- --list --include-ignored > "$RUNNER_TEMP/red-list.txt" 2>&1
fi
red_present=0
for row in "${red_rows[@]}" "$display_row"; do
  if grep -qx "$row: test" "$RUNNER_TEMP/red-list.txt"; then
    red_present=$((red_present + 1))
    log "red row already present: $row"
  fi
done
log "red: build exit $red_built, red rows present $red_present of $((${#red_rows[@]} + 1))"
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

export GTK_IM_MODULE=ibus XMODIFIERS=@im=ibus WEBKIT_DISABLE_DMABUF_RENDERER=1 NO_AT_BRIDGE=1
unset DISPLAY WAYLAND_DISPLAY
ibus-daemon --replace --panel=disable --verbose > "$root/ibus.log" 2>&1 &
ibus_ready=false
for _ in $(seq 1 50); do
  if ibus list-engine > "$root/ibus-engines.txt" 2>&1; then ibus_ready=true; break; fi
  sleep 0.2
done
ibus_address=$(ibus address 2> /dev/null)
if [ -n "$ibus_address" ] && [ "$ibus_address" != "(null)" ]; then
  export IBUS_ADDRESS="$ibus_address"
fi
log "ibus ready $ibus_ready, latn-pre engines $(grep -c 'm17n:t:latn-pre' "$root/ibus-engines.txt" 2> /dev/null)"

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

focus_window() {
  local kind=$1
  [ "$kind" = x11 ] || return 0
  timeout 10 xdotool search --sync --limit 1 --name '^native-row$' windowfocus --sync > /dev/null 2>&1
  log "  $cell window focus exit $?"
}

type_text() {
  local kind=$1 text=$2
  if [ "$kind" = x11 ]; then
    timeout 30 xdotool type --delay 80 "$text"
  else
    timeout 30 wtype -s 300 -d 80 "$text"
  fi
}

type_keys() {
  local kind=$1
  shift
  if [ "$kind" = x11 ]; then
    timeout 30 xdotool key --delay 80 "$@"
  else
    local args=()
    for name in "$@"; do args+=(-k "$name"); done
    timeout 30 wtype -d 80 "${args[@]}"
  fi
}

compose() {
  local kind=$1 step=$2 text=$3 set_rc before after
  timeout 5 ibus engine m17n:t:latn-pre > /dev/null 2>&1
  set_rc=$?
  for _ in $(seq 1 25); do
    before=$(timeout 2 ibus engine 2>&1)
    [ "$before" = m17n:t:latn-pre ] && break
    sleep 0.2
  done
  type_text "$kind" "$text"
  sleep 1
  after=$(timeout 2 ibus engine 2>&1)
  timeout 5 ibus engine xkb:us::eng > /dev/null 2>&1
  log "  $cell $step engine: set exit $set_rc, before typing $before, after typing $after"
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

steps=(page native page-ime native-ime covered)
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
    cell_rows+=("| $cell | display did not start | | |")
    row_ok=1
    stop_display
    return
  fi
  timeout 5 ibus engine xkb:us::eng > /dev/null 2>&1
  [ "$load" = loaded ] && load_on
  NATIVE_ROW_DIR="$dir" timeout 300 cargo test "${pkg[@]}" --lib -- --ignored --exact "$display_row" --test-threads=1 > "$dir/row.txt" 2>&1 &
  local pid=$! step reached=0 last=none
  for step in "${steps[@]}"; do
    if ! wait_step "$dir/$step.json" "$pid"; then
      log "::error::$cell: the row never reached $step"
      break
    fi
    reached=$((reached + 1))
    last=$step
    case "$step" in
      page)
        focus_window "$kind"
        type_text "$kind" xy
        ;;
      native)
        type_text "$kind" ab
        type_keys "$kind" Return Up
        ;;
      page-ime) compose "$kind" "$step" "'e" ;;
      native-ime) compose "$kind" "$step" "'a" ;;
      covered) type_text "$kind" z ;;
    esac
    : > "$dir/go-$step"
  done
  wait "$pid"
  local rc=$?
  load_off
  stop_display
  tail -n 30 "$dir/row.txt" >> "$out"
  local panic
  panic=$(grep -m1 -E "panicked at|assertion" "$dir/row.txt" | cut -c1-160)
  log "$cell: row exit $rc, steps reached $reached of ${#steps[@]} (last $last)"
  cell_rows+=("| $cell | $rc | $reached of ${#steps[@]} ($last) | ${panic:-} |")
  if [ "$rc" != 0 ] || [ "$reached" != "${#steps[@]}" ]; then
    row_ok=1
    cells+=("$cell")
  fi
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

red_ok=1
[ "$red_present" = 0 ] && red_ok=0
{
  printf '| %s | exit |\n| --- | --- |\n' "$os"
  printf '| red build (fails: the rows name linux::view items before they exist) | %s |\n' "$red_built"
  printf '| red rows present at red tip (want 0) | %s of %s |\n' "$red_present" "$((${#red_rows[@]} + 1))"
  printf '| green lib build | %s |\n| green lib rows build | %s |\n' "$lib_built" "$rows_built"
  printf '| native_term rows | %s, %s passed |\n' "$rows_rc" "$passed"
  printf '| named rows ok, none failed, display rows ignored (0 = yes) | %s |\n' "$want"
  printf '| display row cells all pass (0 = yes) | %s %s |\n' "$row_ok" "${cells[*]:-}"
  printf '| clippy --lib -D warnings | %s |\n| clippy --tests -D warnings | %s |\n' "$clip" "$clipt"
  printf '\n| cell | row exit | steps reached | first panic |\n| --- | --- | --- | --- |\n'
  printf '%s\n' "${cell_rows[@]}"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$red_ok" = 0 ] && [ "$lib_built" = 0 ] && [ "$rows_built" = 0 ] && [ "$want" = 0 ] \
  && [ "$rows_rc" = 0 ] && [ "$row_ok" = 0 ] && [ "$clip" = 0 ] && [ "$clipt" = 0 ]
