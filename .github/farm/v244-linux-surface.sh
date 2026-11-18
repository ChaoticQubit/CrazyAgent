#!/usr/bin/env bash
set +e -uo pipefail

os="$1"
case "$os" in
  linux) ;;
  *) echo "::error::the L2 surface rows run on Linux only, not $os"; exit 2 ;;
esac
out="$RUNNER_TEMP/v244-linux-surface-$os.txt"
: > "$out"
root="$RUNNER_TEMP/surface-cells"
mkdir -p "$root"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }

red_rows=(
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
log "os $os red $RED_SHA green $GREEN_SHA $(rustc -V)"
log "weston $(weston --version 2>&1 | head -1); $(sway --version 2>&1 | head -1); $(vulkaninfo --summary 2>/dev/null | grep -m1 deviceName)"

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
  grep -qx "test $display_row ... ignored, needs a display: the farm runs it under Xvfb, weston and sway" "$RUNNER_TEMP/rows.txt" \
    || { log "::error::the display row is not ignored in a plain run"; want=1; }
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
    cell_rows+=("| $cell | display did not start | | | |")
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
  load_off
  stop_display
  tail -n 25 "$dir/row.txt" >> "$out"
  local report=none
  [ -f "$dir/report.json" ] && report=$(jq -c '{present_ms, wait_ms, shown_wait_ms, shown_present_ms}' "$dir/report.json")
  log "$cell: row exit $rc, steps $shots of 3, pixel steps passed $probes_ok of 3, $report"
  local present wait
  present=$(jq -r '.present_ms | "\(.p50*100|round/100) / \(.p95*100|round/100) / \(.max*100|round/100)"' "$dir/report.json" 2> /dev/null || echo none)
  wait=$(jq -r '.wait_ms | "\(.p50*100|round/100) / \(.p95*100|round/100) / \(.max*100|round/100)"' "$dir/report.json" 2> /dev/null || echo none)
  cell_rows+=("| $cell | $rc | $probes_ok of 3 | $present | $wait |")
  if [ "$rc" != 0 ] || [ "$probes_ok" != 3 ]; then
    row_ok=1
    cells+=("$cell")
  fi
}

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

red_ok=1
[ "$red_present" = 0 ] && red_ok=0
{
  printf '| %s | exit |\n| --- | --- |\n' "$os"
  printf '| red build (fails: the row calls Child::presented before it exists) | %s |\n' "$red_built"
  printf '| red rows present at red tip (want 0) | %s of %s |\n' "$red_present" "$((${#red_rows[@]} + 1))"
  printf '| green lib build | %s |\n| green lib rows build | %s |\n' "$lib_built" "$rows_built"
  printf '| native_term rows | %s, %s passed |\n' "$rows_rc" "$passed"
  printf '| named rows ok, none failed (0 = yes) | %s |\n' "$want"
  printf '| display row cells all pass (0 = yes) | %s %s |\n' "$row_ok" "${cells[*]:-}"
  printf '| clippy --lib -D warnings | %s |\n| clippy --tests -D warnings | %s |\n' "$clip" "$clipt"
  printf '\n| cell | row exit | pixel steps | present ms p50 / p95 / max | wait for ready ms p50 / p95 / max |\n| --- | --- | --- | --- | --- |\n'
  printf '%s\n' "${cell_rows[@]}"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$red_ok" = 0 ] && [ "$lib_built" = 0 ] && [ "$rows_built" = 0 ] && [ "$want" = 0 ] \
  && [ "$rows_rc" = 0 ] && [ "$row_ok" = 0 ] && [ "$clip" = 0 ] && [ "$clipt" = 0 ]
