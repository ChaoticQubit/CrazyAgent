#!/bin/bash
set -u
PROBE=${PROBE_DIR:-$(cd "$(dirname "$0")" && pwd)}
OUT=${FLAME_OUT:?}
mkdir -p "$OUT" "$OUT/xtrace"
SUM=$OUT/RUNS.txt
say() { echo "$*" | tee -a "$SUM"; }
load1() { sysctl -n vm.loadavg | awk '{print $2}'; }
ncpu() { sysctl -n hw.ncpu; }
bin_of() { awk -v s="$1" '$1==s{print $2}' "$OUT/BINS.txt"; }
kill_tree() {
  local p=$1 c
  for c in $(pgrep -P "$p"); do kill_tree "$c"; done
  kill -TERM "$p" 2>/dev/null
}

build() {
  local side=$1 dir=$2 pkg=$3 log
  log=$RUNNER_TEMP/build-$side.json
  (cd "$dir" && cargo fetch --locked > /dev/null 2>&1 && CARGO_TARGET_DIR=$RUNNER_TEMP/target-$side CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo test --release --locked -p "$pkg" --lib --no-run --message-format=json > "$log" 2> "$log.err")
  local rc=$?
  local bin
  bin=$(python3 -I -c 'import json,sys
for l in open(sys.argv[1]):
    try: m=json.loads(l)
    except Exception: continue
    if m.get("reason")=="compiler-artifact" and m.get("executable") and m["target"]["kind"]==["lib"] and m["profile"]["test"]: print(m["executable"])' "$log" | tail -1)
  say "BUILD $side $(git -C "$dir" rev-parse --short=10 HEAD) $pkg rc=$rc bin=$bin df=$(df -g "$RUNNER_TEMP" | awk 'NR==2{print $4}')G"
  [ "$rc" = 0 ] && [ -x "$bin" ] || { tail -30 "$log.err"; return 1; }
  echo "$side $bin" >> "$OUT/BINS.txt"
}

quiet_wait() {
  local waited=0 l b
  while :; do
    l=$(load1)
    b=$(pgrep -x cargo rustc | wc -l | tr -d ' ')
    [ "$b" = 0 ] && awk -v l="$l" 'BEGIN{exit !(l<3)}' && { say "QUIET-READY load1=$l builds=0 waited_s=$waited"; return 0; }
    [ "$waited" -ge 1800 ] && { say "QUIET-WAIT cap load1=$l builds=$b after $waited s"; return 1; }
    sleep 15; waited=$((waited+15))
  done
}

loaded_wait() {
  local waited=0 l
  [ -s "$RUNNER_TEMP/loader.pid" ] || { say "LOADER not running"; return 1; }
  while :; do
    l=$(load1)
    awk -v l="$l" -v n="$(ncpu)" 'BEGIN{exit !(l>=n)}' && { say "LOADED-READY load1=$l ncpu=$(ncpu) waited_s=$waited"; return 0; }
    [ "$waited" -ge 900 ] && { say "LOADED-WAIT cap load1=$l ncpu=$(ncpu)"; return 1; }
    sleep 15; waited=$((waited+15))
  done
}

loader() {
  if [ "$1" = start ]; then
    local dir=$2
    (cd "$dir" && while :; do CARGO_TARGET_DIR=$RUNNER_TEMP/loader-target cargo build --release --locked -p crazyagent-kernel-storage > /dev/null 2>&1; rm -rf "$RUNNER_TEMP/loader-target"; done) &
    echo $! > "$RUNNER_TEMP/loader.pid"
    say "LOADER start pid $(cat "$RUNNER_TEMP/loader.pid") load1=$(load1)"
  else
    local p
    p=$(cat "$RUNNER_TEMP/loader.pid" 2>/dev/null)
    [ -n "$p" ] && kill_tree "$p"
    rm -f "$RUNNER_TEMP/loader.pid"
    sleep 5
    say "LOADER stop load1=$(load1) cargo_left=$(pgrep -x cargo | wc -l | tr -d ' ')"
  fi
}

one_sample() {
  local side=$1 test=$2 row=$3 tag=$4 cond=$5 secs=$6 fsecs=$7
  local bin log smp
  bin=$(bin_of "$side")
  log=$OUT/$tag-$cond.log smp=$OUT/$tag-$cond.sample.txt
  [ -x "$bin" ] || { say "GATE FAIL $tag $cond no bin"; return 1; }
  if [ "$cond" = quiet ]; then quiet_wait || { say "GATE FAIL $tag $cond not quiet"; return 1; }; else loaded_wait || { say "GATE FAIL $tag $cond not loaded"; return 1; }; fi
  local s
  s=$(load1)
  CRAZYAGENT_PERF_FLAME="$row" CRAZYAGENT_PERF_FLAME_SECS=$fsecs "$bin" --exact "$test" --ignored --nocapture --test-threads=1 > "$log" 2>&1 &
  local pid=$! t=0
  sleep 0.5
  local comm ni
  comm=$(basename "$(ps -o comm= -p $pid)")
  ni=$(ps -o ni= -p $pid | tr -d ' ')
  [ "$comm" = "$(basename "$bin")" ] || { say "GATE FAIL $tag $cond pid $pid is $comm"; kill $pid; return 1; }
  until grep -q 'FLAME-START ' "$log" || [ "$t" -ge 1500 ] || ! kill -0 $pid 2>/dev/null; do sleep 1; t=$((t+1)); done
  rm -f "$smp"
  grep -q 'FLAME-START ' "$log" && { sleep 1; /usr/bin/sample $pid "$secs" -file "$smp" > /dev/null 2>&1; }
  wait $pid
  local rc=$?
  say "RUN $tag $cond rc=$rc ni=$ni load1_start=$s load1_end=$(load1) $(grep -oE 'FLAME-(START|END) .*' "$log" | tr '\n' ' ')"
  [ "$(grep -o 'FLAME-START ' "$log" | wc -l | tr -d ' ')" = 1 ] && [ "$(grep -o "FLAME-END [^ ]* [^ ]* $row calls=" "$log" | wc -l | tr -d ' ')" = 1 ] && [ -s "$smp" ] || { say "GATE FAIL $tag $cond FLAME lines or no sample"; return 1; }
  grep -q "^Process: *$(basename "$bin") " "$smp" || { say "GATE FAIL $tag $cond sample Process is not the test binary"; return 1; }
  ! grep -qE '__wait4|\[bash\]|\(in bash\)' "$smp" || { say "GATE FAIL $tag $cond __wait4 or bash frame"; return 1; }
  (cd "$PROBE" && python3 -I flame.py "$smp" "$OUT/$tag-$cond.svg" --top 15 --thread "$test" > "$OUT/$tag-$cond.table.txt" 2>&1)
  (cd "$PROBE" && python3 -I flame.py "$smp" "$OUT/$tag-$cond-allthreads.svg" --top 15 > "$OUT/$tag-$cond-allthreads.table.txt" 2>&1)
  python3 -I "$PROBE/split.py" "$OUT/$tag-$cond.folded" > "$OUT/$tag-$cond.split.txt" 2>&1
  local ns
  ns=$(awk 'NR==1{print $2}' "$OUT/$tag-$cond.table.txt")
  say "FLAME $tag $cond samples=$ns secs=$secs"
  [ "${ns:-0}" -ge 5000 ] || { say "GATE LOW $tag $cond samples $ns"; return 2; }
}

sample() {
  local side=$1 test=$2 row=$3 slug=$4 cond=$5 secs=$6
  local fsecs=$((secs + 60))
  one_sample "$side" "$test" "$row" "$slug-$side" "$cond" "$secs" "$fsecs"
  local rc=$?
  if [ "$rc" = 2 ]; then
    say "RETRY $slug-$side $cond with sample secs $((secs * 3))"
    one_sample "$side" "$test" "$row" "$slug-$side" "$cond" $((secs * 3)) $((secs * 3 + 60)) || { say "GATE FAIL $slug-$side $cond after retry"; return 1; }
  elif [ "$rc" != 0 ]; then
    return 1
  fi
}

xrec() {
  local side=$1 test=$2 row=$3 slug=$4 k=$5
  local bin T L Y
  bin=$(bin_of "$side")
  T=$OUT/xtrace/$slug-$side-K$k.trace L=$OUT/xtrace/$slug-$side-K$k.log Y=$OUT/xtrace/$slug-$side-K$k.sym.trace
  rm -rf "${T:?}" "${Y:?}"
  [ -e "$RUNNER_TEMP/disk-stop" ] && { say "XREC VOID $slug-$side K=$k disk stop set"; return 1; }
  local free
  free=$(df -g "$RUNNER_TEMP" | awk 'NR==2{print $4}')
  [ "$free" -ge 10 ] || { touch "$RUNNER_TEMP/disk-stop"; say "DISK STOP before $slug-$side K=$k free ${free}G"; say "XREC VOID $slug-$side K=$k disk"; return 1; }
  local s mode=user
  s=$(load1)
  record() {
    $1 xcrun xctrace record --template 'System Trace' --time-limit 900s --output "$T" --target-stdout "$L" \
      --env CRAZYAGENT_PERF_FLAME="$row" --env CRAZYAGENT_PERF_FLAME_CALLS="$k" --env CRAZYAGENT_PERF_FLAME_SECS=600 \
      --launch -- "$bin" --exact "$test" --ignored --nocapture --test-threads=1 > "$L.xctrace" 2>&1
  }
  record ""
  if [ ! -d "$T" ]; then mode=sudo; record "sudo -E"; sudo chown -R "$(id -u)" "$T" "$L" 2>/dev/null; fi
  local end
  end=$(grep -oE "FLAME-END [^ ]* [^ ]* $row calls=[0-9]+" "$L" 2>/dev/null | grep -oE '[0-9]+$')
  say "XREC $slug-$side K=$k mode=$mode trace=$([ -d "$T" ] && du -sm "$T" | cut -f1)M flame_end_calls=${end:-none} load1 $s->$(load1)"
  [ -d "$T" ] && [ "${end:-x}" = "$k" ] || { say "XREC VOID $slug-$side K=$k"; tail -5 "$L.xctrace" | tee -a "$SUM"; rm -rf "${T:?}"; ktrace_clean "$mode" "$slug-$side K=$k"; return 1; }
  xcrun xctrace symbolicate --input "$T" --output "$Y" --dsym "$bin" > /dev/null 2>&1
  local src=$Y
  [ -d "$Y" ] || src=$T
  xcrun xctrace export --input "$src" --xpath '/trace-toc/run[@number="1"]/data/table[@schema="syscall"]' > "$RUNNER_TEMP/x.xml" 2> "$RUNNER_TEMP/x.err"
  python3 -I "$PROBE/sysc.py" "$RUNNER_TEMP/x.xml" "$(basename "$bin")" > "$OUT/xtrace/$slug-$side-K$k.json"
  local prc=$?
  say "XPARSE $slug-$side K=$k rc=$prc symbolicated=$([ "$src" = "$Y" ] && echo yes || echo no) json=$(wc -c < "$OUT/xtrace/$slug-$side-K$k.json")B"
  rm -rf "${T:?}" "${Y:?}" "$RUNNER_TEMP/x.xml"
  ktrace_clean "$mode" "$slug-$side K=$k"
  [ "$prc" = 0 ]
}

ktrace_clean() {
  local mode=$1 tag=$2 left
  local d dirs
  dirs="${TMPDIR:?} $(getconf DARWIN_USER_TEMP_DIR)"
  [ "$mode" = sudo ] && dirs="$dirs $(sudo getconf DARWIN_USER_TEMP_DIR)"
  for d in $dirs; do
    find "$d" -maxdepth 1 -name 'instruments*.ktrace' -exec rm -f {} + 2>/dev/null
    [ "$mode" = sudo ] && sudo find "$d" -maxdepth 1 -name 'instruments*.ktrace' -exec rm -f {} + 2>/dev/null
  done
  left=0
  for d in $dirs; do left=$((left + $(sudo find "$d" -maxdepth 1 -name 'instruments*.ktrace' 2>/dev/null | wc -l))); done
  local free
  free=$(df -g "$RUNNER_TEMP" | awk 'NR==2{print $4}')
  say "DF after $tag free=${free}G ktrace_left=$left"
  [ "$free" -ge 10 ] || { touch "$RUNNER_TEMP/disk-stop"; say "DISK STOP after $tag free ${free}G"; }
}

"$@"
