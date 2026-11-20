set -uo pipefail
started=$(date +%s)
tip=$(git rev-parse --short=9 HEAD)
echo "N4a #721 C5 timing mode=$MODE at $tip on $ImageOS, $(nproc) cores"
row='engine::native_auth_host::pin_windows_tests::the_time_one_check_takes_for_a_360_mb_harness'
cargo test -p crazyagent --lib --locked --no-run > "$RUNNER_TEMP/build.log" 2>&1 || { grep -E '^error' -A8 "$RUNNER_TEMP/build.log" | head -60; exit 1; }
echo "test build in $(( $(date +%s) - started )) s"
load() { powershell -NoProfile -NonInteractive -Command "(Get-CimInstance Win32_Processor | Measure-Object -Property LoadPercentage -Average).Average" 2>/dev/null | tr -d '\r'; }
probe() {
  local label=$1 n
  for n in 1 2 3; do
    out="$RUNNER_TEMP/probe-$label-$n.txt"
    before=$(load)
    CRAZYAGENT_FARM_PROBE_OUT="$out" cargo test -p crazyagent --lib --locked -- --exact --ignored "$row" > "$RUNNER_TEMP/probe-$label-$n.log" 2>&1
    rc=$?
    echo "$label $n rc=$rc cpu-before=${before}% cpu-after=$(load)% | $(cat "$out" 2>/dev/null)"
    [ "$rc" -eq 0 ] || grep -E -A3 'panicked at' "$RUNNER_TEMP/probe-$label-$n.log" | head -6
  done
}
probe quiet
CARGO_TARGET_DIR="$RUNNER_TEMP/load-target" cargo build -p crazyagent --release --locked > "$RUNNER_TEMP/load.log" 2>&1 &
loader=$!
sleep 90
echo "load build pid $loader running: $(kill -0 $loader 2>/dev/null && echo yes || echo no), cpu $(load)%"
probe loaded
echo "load build still running at the end: $(kill -0 $loader 2>/dev/null && echo yes || echo no)"
kill $loader 2>/dev/null
taskkill //F //IM cargo.exe > /dev/null 2>&1
taskkill //F //IM rustc.exe > /dev/null 2>&1
echo "end in $(( $(date +%s) - started )) s"
