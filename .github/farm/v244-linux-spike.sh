set -uo pipefail
if [ -z "${SPIKE_IN_SESSION:-}" ]; then
  exec dbus-run-session -- env SPIKE_IN_SESSION=1 bash "$0" "$@"
fi
summary="${GITHUB_STEP_SUMMARY:-/dev/null}"
root="${RUNNER_TEMP:?}/spike"
app="${GITHUB_WORKSPACE:?}/spikes/linux-native/target/release/linux-native-spike"
results="$root/results.jsonl"
typed=abcdefghijklmnopqrstuvwxyzabcdefghijklmn
mkdir -p "$root"
: > "$results"
[ -x "$app" ] || { echo "::error::no spike binary at $app"; exit 1; }

record() {
  jq -nc --arg cell "$1" --arg row "$2" --argjson value "$3" '{row:$row, cell:$cell, value:$value}' >> "$results" ||
    jq -nc --arg cell "$1" --arg row "$2" --arg raw "$3" '{row:$row, cell:$cell, value:{unparsed:$raw}}' >> "$results"
}

cat > "$root/a11y.py" <<'PY'
import json
import pyatspi

wanted = "crazyagent mirror line one"
found = []
names = []


def walk(node, depth):
    if node is None or depth > 60 or len(found) >= 5:
        return
    try:
        name = node.name or ""
        text = ""
        try:
            body = node.queryText()
            text = body.getText(0, min(body.characterCount, 200))
        except Exception:
            text = ""
        if wanted in name or wanted in text:
            found.append({"role": node.getRoleName(), "name": name[:80], "text": text[:80], "depth": depth})
        for index in range(node.childCount):
            walk(node.getChildAtIndex(index), depth + 1)
    except Exception as error:
        found.append({"error": str(error)[:120], "depth": depth})


desktop = pyatspi.Registry.getDesktop(0)
for index in range(desktop.childCount):
    child = desktop.getChildAtIndex(index)
    if child is not None:
        names.append(child.name)
        walk(child, 0)
print(json.dumps({"apps": names[:20], "found": found, "pass": any("role" in item for item in found)}))
PY

/usr/libexec/at-spi-bus-launcher --launch-immediately > "$root/at-spi.log" 2>&1 &
export GTK_IM_MODULE=ibus XMODIFIERS=@im=ibus GNOME_ACCESSIBILITY=1 WEBKIT_DISABLE_DMABUF_RENDERER=1
unset DISPLAY WAYLAND_DISPLAY
ibus-daemon --replace --panel=disable --verbose > "$root/ibus.log" 2>&1 &
ibus_ready=false
for _ in $(seq 1 50); do
  if ibus list-engine > "$root/ibus-engines.txt" 2>&1; then ibus_ready=true; break; fi
  sleep 0.2
done
ibus_address=$(ibus address 2> /dev/null)
address_set=false
if [ -n "$ibus_address" ] && [ "$ibus_address" != "(null)" ]; then
  export IBUS_ADDRESS="$ibus_address"
  address_set=true
fi
latn_pre=$(grep -c 'm17n:t:latn-pre' "$root/ibus-engines.txt" 2> /dev/null)
record setup ibus "$(jq -nc --argjson ready "$ibus_ready" --argjson address "$address_set" --arg latn "${latn_pre:-0}" '{ready:$ready, address_exported:$address, latn_pre_engines:$latn}')"
record setup versions "$(jq -nc --arg weston "$(weston --version 2>&1 | head -1)" --arg sway "$(sway --version 2>&1 | head -1)" --arg vulkan "$(vulkaninfo --summary 2>&1 | grep -m3 -E 'deviceName|driverName|apiVersion' | tr -s ' ' | tr '\n' ';')" --arg webkit "$(dpkg-query -W -f='${Version}' libwebkit2gtk-4.1-0 2>&1)" --arg ibus "$(ibus version 2>&1 | head -1)" '{weston:$weston, sway:$sway, vulkan:$vulkan, webkit:$webkit, ibus:$ibus}')"

display_pid=""
load_pids=()

start_x11() {
  Xvfb :99 -screen 0 "$1x24" -nolisten tcp -noreset > "$root/xvfb-$cell.log" 2>&1 &
  display_pid=$!
  export DISPLAY=:99
  unset WAYLAND_DISPLAY
  for _ in $(seq 1 50); do
    xdotool getdisplaygeometry > /dev/null 2>&1 && return 0
    sleep 0.1
  done
  return 1
}

start_weston() {
  export XDG_RUNTIME_DIR="$RUNNER_TEMP/xdg-$cell"
  mkdir -p -m 700 "$XDG_RUNTIME_DIR"
  weston --socket=wayland-spike --width=1280 --height=800 --idle-time=0 --debug --backend=headless --renderer=pixman --shell=kiosk > "$root/weston-$cell.log" 2>&1 &
  display_pid=$!
  export WAYLAND_DISPLAY=wayland-spike
  unset DISPLAY
  for _ in $(seq 1 50); do
    [ -S "$XDG_RUNTIME_DIR/wayland-spike" ] && return 0
    sleep 0.1
  done
  return 1
}

start_sway() {
  command -v sway > /dev/null || return 1
  export XDG_RUNTIME_DIR="$RUNNER_TEMP/xdg-$cell"
  mkdir -p -m 700 "$XDG_RUNTIME_DIR"
  printf '%s\n' 'output HEADLESS-1 resolution 1280x800 position 0 0' 'default_border none' 'default_floating_border none' 'focus_follows_mouse no' > "$root/sway.conf"
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
  local cores
  cores=$(nproc)
  for _ in $(seq 1 "$cores"); do
    yes > /dev/null &
    load_pids+=($!)
  done
  record setup load "$(jq -nc --argjson cores "$cores" '{busy_loops:$cores}')"
}

load_off() {
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
  local cell=$1 name=$2 file=$3 x=$4 y=$5 want=$6 got
  got=$(convert "$file" -crop "1x1+${x}+${y}" +repage -format '%[fx:int(255*r+0.5)],%[fx:int(255*g+0.5)],%[fx:int(255*b+0.5)]' info: 2> /dev/null || echo unreadable)
  record "$cell" "pixel_$name" "$(jq -nc --arg got "$got" --arg want "$want" --argjson x "$x" --argjson y "$y" '{x:$x, y:$y, got:$got, want:$want, pass:($got == $want)}')"
}

geometry_of() {
  grep '^SPIKE ' "$1/app.log" | cut -c7- | jq -c 'select(.row == "placed" or .row == "moved") | .value.placed' | tail -1
}

pixels() {
  local cell=$1 kind=$2 step=$3 out=$4
  local file="$out/$step.png"
  shot "$kind" "$file" || { record "$cell" "pixels_$step" '{"shot":false}'; return; }
  local geometry
  geometry=$(geometry_of "$out")
  if [ -z "$geometry" ] || [ "$geometry" = null ]; then
    record "$cell" "pixels_$step" '{"geometry":false}'
    return
  fi
  local ox oy w h s
  read -r ox oy w h s < <(jq -r '[(.root[0] // .device[0]), (.root[1] // .device[1]), .device[2], .device[3], .scale] | @tsv' <<< "$geometry")
  case "$step" in
    placed)
      probe "$cell" placed_surface "$file" $((ox + w / 2)) $((oy + h / 2)) 0,255,0
      probe "$cell" placed_page "$file" $((ox - 15 * s)) $((oy + 10 * s)) 255,255,255
      ;;
    moved)
      probe "$cell" moved_surface "$file" $((ox + w / 2)) $((oy + h / 2)) 0,255,0
      probe "$cell" moved_old_area "$file" $((ox - 50 * s)) $((oy + 10 * s)) 255,255,255
      ;;
    hidden)
      probe "$cell" hidden_popover "$file" $((ox + 240 * s)) $((oy + 130 * s)) 255,255,0
      probe "$cell" hidden_host "$file" $((ox + 20 * s)) $((oy + 20 * s)) 255,0,255
      ;;
  esac
}

focus_app() {
  timeout 10 xdotool search --sync --limit 1 --name '^linux-native-spike$' windowfocus --sync > /dev/null 2>&1
}

type_keys() {
  local kind=$1 text=$2 delay=$3
  if [ "$kind" = x11 ]; then
    focus_app
    xdotool type --delay "$delay" "$text"
  else
    timeout 30 wtype -d "$delay" "$text"
  fi
}

click_host() {
  local cell=$1 kind=$2 out=$3 geometry cx cy
  if [ "$kind" != x11 ]; then
    record "$cell" click_driver '{"measurable":false,"why":"no pointer driver on this compositor"}'
    return
  fi
  geometry=$(geometry_of "$out")
  if [ -z "$geometry" ] || [ "$geometry" = null ]; then
    record "$cell" click_driver '{"geometry":false}'
    return
  fi
  read -r cx cy < <(jq -r '[((.root[0] // .device[0]) + (.device[2] / 2 | floor)), ((.root[1] // .device[1]) + (.device[3] / 2 | floor))] | @tsv' <<< "$geometry")
  timeout 10 xdotool mousemove --sync "$cx" "$cy" click 1
  sleep 0.5
  record "$cell" click_driver "$(jq -nc --argjson x "$cx" --argjson y "$cy" '{measurable:true, x:$x, y:$y}')"
}

ime() {
  local cell=$1 kind=$2 step=$3 set_out set_rc now="" after
  set_out=$(timeout 5 ibus engine m17n:t:latn-pre 2>&1)
  set_rc=$?
  for _ in $(seq 1 25); do
    now=$(timeout 2 ibus engine 2>&1)
    [ "$now" = m17n:t:latn-pre ] && break
    sleep 0.2
  done
  type_keys "$kind" "'e'a'o" 80
  sleep 1
  after=$(timeout 2 ibus engine 2>&1)
  timeout 5 ibus engine xkb:us::eng > /dev/null 2>&1
  record "$cell" "${step}_engine" "$(jq -nc --arg out "$set_out" --argjson rc "$set_rc" --arg before "$now" --arg after "$after" '{set_rc:$rc, set_out:$out, before_typing:$before, after_typing:$after, active:($before == "m17n:t:latn-pre")}')"
}

clipboard() {
  local cell=$1 kind=$2 clip prim
  if [ "$kind" = x11 ]; then
    clip=$(timeout 5 xclip -o -selection clipboard 2>&1)
    prim=$(timeout 5 xclip -o -selection primary 2>&1)
    printf 'pasted-into-app' | timeout 5 xclip -i -selection clipboard
    printf 'primary-into-app' | timeout 5 xclip -i -selection primary
  else
    clip=$(timeout 5 wl-paste -n 2>&1)
    prim=$(timeout 5 wl-paste -p -n 2>&1)
    timeout 5 wl-copy 'pasted-into-app'
    timeout 5 wl-copy -p 'primary-into-app'
  fi
  record "$cell" clip_out "$(jq -nc --arg clip "$clip" --arg prim "$prim" '{clipboard:$clip, primary:$prim, clipboard_pass:($clip == "crazyagent-clipboard-row"), primary_pass:($prim == "crazyagent-primary-row")}')"
}

act() {
  local cell=$1 kind=$2 step=$3 out=$4
  case "$step" in
    placed | moved | hidden) pixels "$cell" "$kind" "$step" "$out" ;;
    click) click_host "$cell" "$kind" "$out" ;;
    type_a | type_b)
      type_keys "$kind" "$typed" 40
      sleep 1
      ;;
    ime_a | ime_b) ime "$cell" "$kind" "$step" ;;
    clip_out) clipboard "$cell" "$kind" ;;
    a11y) record "$cell" a11y "$(timeout 30 python3 "$root/a11y.py" 2>&1 | tail -1)" ;;
    *) record "$cell" unknown_checkpoint "$(jq -nc --arg step "$step" '{step:$step}')" ;;
  esac
}

backtrace() {
  local out=$1 parent=$2 child
  child=$(pgrep -P "$parent" | head -1)
  [ -n "$child" ] || return
  sudo timeout 30 gdb -batch -iex 'set debuginfod enabled off' -p "$child" -ex 'thread apply all bt' > "$out/gdb.txt" 2>&1
}

run_cell() {
  cell=$1
  local kind=$2 gpu=$3 sync=$4 present=$5 scale=$6 steps=$7 limit=${8:-120}
  local out="$root/$cell" backend=wayland debug="" size=1280x800 started dumped=false
  mkdir -p "$out"
  [ "$kind" = x11 ] && backend=x11
  if [ "$kind" != x11 ] && [ "${sync#desync}" != "$sync" ]; then debug=client; fi
  [ "$scale" = 2 ] && size=2560x1600
  case "$kind" in
    x11) start_x11 "$size" ;;
    weston) start_weston ;;
    sway) start_sway ;;
  esac || {
    record "$cell" display '{"started":false}'
    stop_display
    return
  }
  started=$(date +%s)
  env SPIKE_OUT="$out" SPIKE_CELL="$cell" SPIKE_GPU="$gpu" SPIKE_SYNC="$sync" SPIKE_PRESENT="$present" \
    SPIKE_STEPS="$steps" GDK_BACKEND="$backend" GDK_SCALE="$scale" ${debug:+WAYLAND_DEBUG=$debug} \
    timeout "$limit" "$app" > "$out/app.log" 2>&1 &
  local app_pid=$!
  while kill -0 "$app_pid" 2> /dev/null; do
    for ready in "$out"/ready-*; do
      [ -e "$ready" ] || continue
      local step=${ready##*/ready-}
      rm -f "$ready"
      act "$cell" "$kind" "$step" "$out"
      : > "$out/go-$step"
    done
    if [ "$dumped" = false ] && [ $(($(date +%s) - started)) -ge $((limit - 10)) ]; then
      dumped=true
      backtrace "$out" "$app_pid"
    fi
    sleep 0.1
  done
  wait "$app_pid"
  local rc=$?
  grep '^SPIKE ' "$out/app.log" | cut -c7- >> "$results"
  record "$cell" exit "$(jq -nc --argjson rc "$rc" --argjson seconds "$(($(date +%s) - started))" --argjson dumped "$dumped" '{rc:$rc, seconds:$seconds, backtrace:$dumped}')"
  if [ "$rc" -ne 0 ]; then
    echo "::group::$cell app.log tail (rc $rc)"
    grep -v '^\[' "$out/app.log" | tail -n 40
    echo "::endgroup::"
  fi
  stop_display
}

run_cell x11-vulkan x11 vulkan sync vsync 1 placed,moved,hidden,click,type_a,type_b,ime_a,ime_b,clip,a11y
run_cell x11-gl x11 gl sync vsync 1 placed,moved,hidden,click
run_cell x11-vulkan-scale2 x11 vulkan sync vsync 2 placed,moved,hidden
run_cell wl-vulkan-sync weston vulkan sync vsync 1 placed,moved,hidden,clip
run_cell wl-gl-sync weston gl sync vsync 1 placed,moved,hidden
run_cell wl-vulkan-desync weston vulkan desync vsync 1 placed 40
run_cell wl-vulkan-desync-immediate weston vulkan desync immediate 1 placed 40
run_cell wl-vulkan-desync-first weston vulkan desync-first vsync 1 placed 40
run_cell wl-gl-desync weston gl desync vsync 1 placed 40
run_cell sway-vulkan-sync sway vulkan sync vsync 1 placed,moved,hidden,type_a,type_b,ime_a,ime_b,clip
run_cell sway-vulkan-desync sway vulkan desync vsync 1 placed 40
load_on
run_cell x11-vulkan-loaded x11 vulkan sync vsync 1 placed,type_a,type_b
run_cell wl-vulkan-sync-loaded weston vulkan sync vsync 1 placed
run_cell sway-vulkan-sync-loaded sway vulkan sync vsync 1 placed,type_a,type_b
load_off

{
  printf '## Native terminal Linux embed spike\n\nSoftware rendering only (lavapipe / llvmpipe on a GitHub runner). WEBKIT_DISABLE_DMABUF_RENDERER=1. Frames only clear the surface: they time the present path, not glyph drawing. Cells ending in -loaded run with every core busy.\n\n'
  printf '| cell | row | result |\n| --- | --- | --- |\n'
  jq -r '[.cell, .row, (.value | tostring | .[0:400] | gsub("\\|"; "/"))] | "| \(.[0]) | \(.[1]) | \(.[2]) |"' "$results"
  printf '\npixel rows passed: %s of %s\n' "$(jq -s '[.[] | select(.row | startswith("pixel_")) | select(.value.pass)] | length' "$results")" "$(jq -s '[.[] | select(.row | startswith("pixel_"))] | length' "$results")"
} >> "$summary"
exit 0
