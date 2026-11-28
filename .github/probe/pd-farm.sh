#!/bin/bash
PD_DIR=$(cd "$(dirname "$0")" && pwd)
PROBE_DIR=$PD_DIR
source "$PD_DIR/flame-farm.sh" true

kc_one() {
  local side=$1 test=$2 row=$3 tag=$4 cond=$5 secs=$6 fsecs=$7
  local bin log smp
  bin=$(bin_of "$side")
  log=$OUT/$tag-$cond.log smp=$OUT/$tag-$cond.sample.txt
  [ -x "$bin" ] || { say "GATE FAIL $tag $cond no bin"; return 1; }
  if [ "$cond" = quiet ]; then quiet_wait; else loaded_wait || { say "GATE FAIL $tag $cond not loaded"; return 1; }; fi
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
  ! grep -qE '\[bash\]|\(in bash\)' "$smp" || { say "GATE FAIL $tag $cond bash frame"; return 1; }
  (cd "$PD_DIR" && python3 -I flame.py "$smp" "$OUT/$tag-$cond.svg" --top 15 --thread "$test" > "$OUT/$tag-$cond.table.txt" 2>&1)
  (cd "$PD_DIR" && python3 -I flame.py "$smp" "$OUT/$tag-$cond-allthreads.svg" --top 15 > "$OUT/$tag-$cond-allthreads.table.txt" 2>&1)
  python3 -I "$PD_DIR/split.py" "$OUT/$tag-$cond.folded" > "$OUT/$tag-$cond.split.txt" 2>&1
  local ns
  ns=$(awk 'NR==1{print $2}' "$OUT/$tag-$cond.table.txt")
  say "FLAME $tag $cond samples=$ns secs=$secs"
  [ "${ns:-0}" -ge 5000 ] || { say "GATE LOW $tag $cond samples $ns"; return 2; }
}

kc_test() {
  if [ "$1" = twin ]; then echo perf_tests::security_keychain::security_keychain_calls; else echo perf_tests::keychain::keychain_calls; fi
}

kc_run() {
  local side=$1 tag=$2 kind=$3 row=$4 cond=$5 calls=$6 other=k25 rc
  [ "$side" = k25 ] && other=twin
  unset PROBE_KEYCHAIN_SAVE PROBE_KEYCHAIN_FROM CRAZYAGENT_PERF_FLAME_CALLS
  [ "$calls" = - ] || export CRAZYAGENT_PERF_FLAME_CALLS=$calls
  case $kind in
    save) export PROBE_KEYCHAIN_SAVE=$RUNNER_TEMP/kc-$side.keychain-db ;;
    swap) export PROBE_KEYCHAIN_FROM=$RUNNER_TEMP/kc-$other.keychain-db ;;
  esac
  kc_one "$side" "$(kc_test "$side")" "$row" "$tag-$side" "$cond" 8 14
  rc=$?
  unset PROBE_KEYCHAIN_SAVE PROBE_KEYCHAIN_FROM CRAZYAGENT_PERF_FLAME_CALLS
  say "KC $tag-$side $cond kind=$kind row=$row calls=$calls rc=$rc $(grep -oE "FLAME-END .* $row calls=[0-9]+ ns=[0-9]+" "$OUT/$tag-$side-$cond.log" | tail -1)"
  [ "$rc" != 1 ]
}

kc_save() {
  local cond=$1 side
  for side in k25 twin; do
    kc_run "$side" "save" save store.macos.Keychain.put "$cond" - || return 1
    [ -s "$RUNNER_TEMP/kc-$side.keychain-db" ] || { say "GATE FAIL save $side $cond no keychain copy"; return 1; }
  done
  say "KC SAVED $cond $(stat -f '%z' "$RUNNER_TEMP/kc-k25.keychain-db") $(stat -f '%z' "$RUNNER_TEMP/kc-twin.keychain-db") cmp=$(cmp -s "$RUNNER_TEMP/kc-k25.keychain-db" "$RUNNER_TEMP/kc-twin.keychain-db" && echo same || echo differ)"
}

kc_abba() {
  local kind=$1 row=$2 slug=$3 cond=$4 calls=$5 pos=0 side
  for side in k25 twin; do kc_run "$side" "warm-$kind-$slug" "$kind" "$row" "$cond" "$calls"; done
  for side in k25 twin twin k25 k25 twin twin k25; do
    pos=$((pos + 1))
    kc_run "$side" "$kind$pos-$slug" "$kind" "$row" "$cond" "$calls"
  done
}

sdk_test() {
  if [ "$1" = twin ]; then echo perf_tests::sdk::sdk_provider_calls; else echo perf_tests::sdk_provider_calls; fi
}

sdk_sample() {
  local side=$1 cond=$2 secs=$3 bin test tag log smp pid t=0 s rc
  bin=$(bin_of "$side")
  test=$(sdk_test "$side")
  tag=secrets-$side
  log=$OUT/$tag-$cond.log
  smp=$OUT/$tag-$cond.sample.txt
  [ -x "$bin" ] || { say "GATE FAIL $tag $cond no bin"; return 1; }
  if [ "$cond" = quiet ]; then quiet_wait; else loaded_wait || { say "GATE FAIL $tag $cond not loaded"; return 1; }; fi
  s=$(load1)
  "$bin" --exact "$test" --ignored --nocapture --test-threads=1 > "$log" 2>&1 &
  pid=$!
  until pgrep -P "$pid" -x security > /dev/null || [ "$t" -ge 15000 ] || ! kill -0 "$pid" 2>/dev/null; do sleep 0.02; t=$((t + 1)); done
  rm -f "$smp"
  if kill -0 "$pid" 2>/dev/null && [ "$t" -lt 15000 ]; then
    sleep 1
    /usr/bin/sample "$pid" "$secs" -file "$smp" > /dev/null 2>&1
  fi
  wait "$pid"
  rc=$?
  say "RUN $tag $cond rc=$rc wait_polls=$t load1_start=$s load1_end=$(load1) $(grep -oE 'PERF .*provider\.secrets\.(put_from_owner|find|forget) .*' "$log" | tr '\n' ' ')"
  [ "$rc" = 0 ] && [ -s "$smp" ] || { say "GATE FAIL $tag $cond rc or no sample"; return 1; }
  grep -q "^Process: *$(basename "$bin") " "$smp" || { say "GATE FAIL $tag $cond sample Process is not the test binary"; return 1; }
  grep -q 'secrets_calls' "$smp" || { say "GATE FAIL $tag $cond no secrets_calls frame in the sample"; return 1; }
  (cd "$PD_DIR" && python3 -I flame.py "$smp" "$OUT/$tag-$cond.svg" --top 15 --thread "$test" > "$OUT/$tag-$cond.table.txt" 2>&1)
  python3 -I "$PD_DIR/split.py" "$OUT/$tag-$cond.folded" > "$OUT/$tag-$cond.split.txt" 2>&1
  say "FLAME $tag $cond samples=$(awk 'NR==1{print $2}' "$OUT/$tag-$cond.table.txt") secs=$secs"
}

legs() {
  local cond=$1
  kc_save "$cond" || return 1
  kc_abba own store.macos.Keychain.put put "$cond" -
  kc_abba swap store.macos.Keychain.put put "$cond" -
  kc_abba own store.macos.Keychain.delete delete "$cond" 200
  kc_abba swap store.macos.Keychain.delete delete "$cond" 200
  for side in s25 twin s25 twin; do sdk_sample "$side" "$cond" 20; done
}

syscalls() {
  local row slug k side
  for r in "store.macos.Keychain.put put 200" "store.macos.Keychain.get get 200" "store.macos.Keychain.delete delete 200"; do
    read -r row slug k <<< "$r"
    for side in k25 twin; do
      xrec "$side" "$(kc_test "$side")" "$row" "$slug" "$k"
      xrec "$side" "$(kc_test "$side")" "$row" "$slug" 0
    done
  done
}

"$@"
