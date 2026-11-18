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
ibus-daemon --daemonize --replace --panel=disable > "$root/ibus.log" 2>&1
ibus_ready=false
for _ in $(seq 1 50); do
  if ibus list-engine > "$root/ibus-engines.txt" 2>&1; then ibus_ready=true; break; fi
  sleep 0.2
done
latn_pre=$(grep -c 'm17n:t:latn-pre' "$root/ibus-engines.txt" 2>/dev/null)
record setup ibus "$(jq -nc --argjson ready "$ibus_ready" --arg latn "${latn_pre:-0}" '{ready:$ready, latn_pre_engines:$latn}')"
record setup versions "$(jq -nc --arg weston "$(weston --version 2>&1 | head -1)" --arg vulkan "$(vulkaninfo --summary 2>&1 | grep -m3 -E 'deviceName|driverName|apiVersion' | tr -s ' ' | tr '\n' ';')" --arg webkit "$(dpkg-query -W -f='${Version}' libwebkit2gtk-4.1-0 2>&1)" '{weston:$weston, vulkan:$vulkan, webkit:$webkit}')"

display_pid=""

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

start_wayland() {
  export XDG_RUNTIME_DIR="$RUNNER_TEMP/xdg-$cell"
  mkdir -p -m 700 "$XDG_RUNTIME_DIR"
  local major
  major=$(weston --version 2>&1 | grep -oE '[0-9]+' | head -1)
  local flags=(--socket=wayland-spike --width=1280 --height=800 --idle-time=0 --debug)
  if [ "${major:-0}" -ge 10 ]; then
    flags+=(--backend=headless --renderer=pixman --shell=kiosk)
  else
    flags+=(--backend=headless-backend.so --use-pixman --shell=kiosk-shell.so)
  fi
  weston "${flags[@]}" > "$root/weston-$cell.log" 2>&1 &
  display_pid=$!
  export WAYLAND_DISPLAY=wayland-spike
  unset DISPLAY
  for _ in $(seq 1 50); do
    [ -S "$XDG_RUNTIME_DIR/wayland-spike" ] && return 0
    sleep 0.1
  done
  return 1
}

stop_display() {
  [ -n "$display_pid" ] && kill "$display_pid" 2> /dev/null
  wait "$display_pid" 2> /dev/null
  display_pid=""
}

shot() {
  local kind=$1 file=$2
  if [ "$kind" = x11 ]; then
    xwd -root -silent > "$file.xwd" && convert "xwd:$file.xwd" "$file"
  else
    local shots="$root/shots"
    mkdir -p "$shots"
    rm -f "$shots"/*.png
    (cd "$shots" && timeout 10 weston-screenshooter > /dev/null 2>&1) || return 1
    local taken
    taken=$(ls "$shots"/*.png 2> /dev/null | head -1)
    [ -n "$taken" ] && mv "$taken" "$file"
  fi
}

probe() {
  local cell=$1 name=$2 file=$3 x=$4 y=$5 want=$6 got
  got=$(convert "$file" -crop "1x1+${x}+${y}" +repage -format '%[fx:int(255*r+0.5)],%[fx:int(255*g+0.5)],%[fx:int(255*b+0.5)]' info: 2> /dev/null || echo unreadable)
  record "$cell" "pixel_$name" "$(jq -nc --arg got "$got" --arg want "$want" --argjson x "$x" --argjson y "$y" '{x:$x, y:$y, got:$got, want:$want, pass:($got == $want)}')"
}

geometry_of() {
  grep '^SPIKE ' "$1/app.log" | cut -c7- | jq -c 'select(.row == "placed" or .row == "moved") | .value.placed' | tail -1
}

click_host() {
  local cell=$1 kind=$2 out=$3 geometry cx cy
  if [ "$kind" != x11 ]; then
    record "$cell" click_driver '{"measurable":false,"why":"weston headless has no pointer device to drive"}'
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

clipboard() {
  local cell=$1 kind=$2 clip prim measurable=true
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
    if grep -qiE 'seat|not supported|compositor' <<< "$clip$prim"; then measurable=false; fi
  fi
  record "$cell" clip_out "$(jq -nc --arg clip "$clip" --arg prim "$prim" --argjson measurable "$measurable" '{measurable:$measurable, clipboard:$clip, primary:$prim, clipboard_pass:($clip == "crazyagent-clipboard-row"), primary_pass:($prim == "crazyagent-primary-row")}')"
}

act() {
  local cell=$1 kind=$2 step=$3 out=$4
  case "$step" in
    placed | moved | hidden) pixels "$cell" "$kind" "$step" "$out" ;;
    click) click_host "$cell" "$kind" "$out" ;;
    type_a | type_b)
      focus_app
      xdotool type --delay 40 "$typed"
      sleep 1
      ;;
    ime_a | ime_b)
      focus_app
      ibus engine m17n:t:latn-pre
      sleep 0.5
      xdotool type --delay 80 "'e'a'o"
      sleep 1
      ibus engine xkb:us::eng
      ;;
    clip_out) clipboard "$cell" "$kind" ;;
    a11y) record "$cell" a11y "$(timeout 30 python3 "$root/a11y.py" 2>&1 | tail -1)" ;;
    *) record "$cell" unknown_checkpoint "$(jq -nc --arg step "$step" '{step:$step}')" ;;
  esac
}

run_cell() {
  cell=$1
  local kind=$2 gpu=$3 sync=$4 present=$5 scale=$6 steps=$7
  local out="$root/$cell"
  mkdir -p "$out"
  local size=1280x800
  [ "$scale" = 2 ] && size=2560x1600
  local started
  if [ "$kind" = x11 ]; then
    start_x11 "$size" || { record "$cell" display '{"started":false}'; stop_display; return; }
  else
    start_wayland || { record "$cell" display '{"started":false}'; stop_display; return; }
  fi
  started=$(date +%s)
  env SPIKE_OUT="$out" SPIKE_CELL="$cell" SPIKE_GPU="$gpu" SPIKE_SYNC="$sync" SPIKE_PRESENT="$present" \
    SPIKE_STEPS="$steps" GDK_BACKEND="$kind" GDK_SCALE="$scale" timeout 120 "$app" > "$out/app.log" 2>&1 &
  local app_pid=$!
  while kill -0 "$app_pid" 2> /dev/null; do
    for ready in "$out"/ready-*; do
      [ -e "$ready" ] || continue
      local step=${ready##*/ready-}
      rm -f "$ready"
      act "$cell" "$kind" "$step" "$out"
      : > "$out/go-$step"
    done
    sleep 0.1
  done
  wait "$app_pid"
  local rc=$?
  grep '^SPIKE ' "$out/app.log" | cut -c7- >> "$results"
  record "$cell" exit "$(jq -nc --argjson rc "$rc" --argjson seconds "$(($(date +%s) - started))" '{rc:$rc, seconds:$seconds}')"
  if [ "$rc" -ne 0 ]; then
    echo "::group::$cell app.log tail (rc $rc)"
    tail -n 40 "$out/app.log"
    echo "::endgroup::"
  fi
  stop_display
}

run_cell x11-vulkan-plain x11 vulkan sync vsync 1 plain
run_cell x11-gl-plain x11 gl sync vsync 1 plain
run_cell wl-vulkan-plain wayland vulkan sync vsync 1 plain
run_cell wl-gl-plain wayland gl sync vsync 1 plain
run_cell x11-vulkan x11 vulkan sync vsync 1 placed,moved,hidden,click,type_a,type_b,ime_a,ime_b,clip,a11y
run_cell x11-gl x11 gl sync vsync 1 placed,moved,hidden,click
run_cell x11-vulkan-scale2 x11 vulkan sync vsync 2 placed,moved,hidden
run_cell x11-vulkan-novsync x11 vulkan sync immediate 1 placed
run_cell wl-vulkan-sync wayland vulkan sync vsync 1 placed,moved,hidden,click,clip
run_cell wl-vulkan-desync wayland vulkan desync vsync 1 placed,moved,hidden
run_cell wl-gl-sync wayland gl sync vsync 1 placed,moved,hidden

{
  printf '## Native terminal Linux embed spike\n\nSoftware rendering only (lavapipe / llvmpipe on a GitHub runner). WEBKIT_DISABLE_DMABUF_RENDERER=1. Frames only clear the surface: they time the present path, not glyph drawing.\n\n'
  printf '| cell | row | result |\n| --- | --- | --- |\n'
  jq -r '[.cell, .row, (.value | tostring | .[0:400] | gsub("\\|"; "/"))] | "| \(.[0]) | \(.[1]) | \(.[2]) |"' "$results"
  printf '\npixel rows passed: %s of %s\n' "$(jq -s '[.[] | select(.row | startswith("pixel_")) | select(.value.pass)] | length' "$results")" "$(jq -s '[.[] | select(.row | startswith("pixel_"))] | length' "$results")"
} >> "$summary"
exit 0
