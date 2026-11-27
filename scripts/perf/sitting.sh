set -uo pipefail

T="$RUNNER_TEMP/sitting"
R="$T/rc"
mkdir -p "$T" "$R" "$T/sprint/slots" "$T/logs"

note_rc() {
  echo "$2" > "$R/$1"
  echo "== $1 rc $2"
}

run_logged() {
  local label=$1 rc=0
  shift
  "$@" > "$T/logs/$label.log" 2>&1 || rc=$?
  cat "$T/logs/$label.log"
  note_rc "$label" "$rc"
}

load1() {
  python3 -I -c 'import os; print(f"{os.getloadavg()[0]:.2f}")'
}

free_gb() {
  df -g "$RUNNER_TEMP" | awk 'NR == 2 { print $4 }'
}

packages_of() {
  local dir=$1 tests manifest
  for tests in "$dir"/crates/*/src/perf_tests.rs "$dir"/crates/*/src/perf_tests "$dir"/crates/*/*/src/perf_tests.rs "$dir"/crates/*/*/src/perf_tests; do
    [ -e "$tests" ] || continue
    manifest=${tests%/src/*}/Cargo.toml
    printf -- '-p\n%s\n' "$(awk -F'"' '/^name = / { print $2; exit }' "$manifest")"
  done
}

gate() {
  local label=$1 pattern=$2 log=$3 found
  found=$(grep -cE "$pattern" "$log" || true)
  echo "gate $label: $found lines of '$pattern'"
  if [ "$found" -ge 1 ]; then note_rc "gate-$label" 0; else note_rc "gate-$label" 1; fi
}

stamp() {
  echo "STAMP $1 date=$(date -u +%Y-%m-%dT%H:%M:%SZ) load1=$(load1) free_gb=$(free_gb)"
}

case "$1" in
  setup)
    echo "image ${ImageOS:-?} ${ImageVersion:-?} run ${GITHUB_RUN_ID:-?} attempt ${GITHUB_RUN_ATTEMPT:-?} ref ${GITHUB_REF:-?} sha ${GITHUB_SHA:-?}"
    sw_vers || true
    sysctl -n hw.ncpu hw.memsize machdep.cpu.brand_string || true
    for pair in a=49bf8c2c01 twin=05a762238c b-after=68721ffb41 b-before=c6f1398838 load=008adcd803; do
      name=${pair%%=*}
      rev=${pair#*=}
      rc=0
      git worktree add -q --detach "$T/$name" "$rev" || rc=$?
      note_rc "worktree-$name" "$rc"
      echo "side $name commit $(git -C "$T/$name" rev-parse --short=10 HEAD)"
    done
    ln -s "$T/a" "$T/a-aa"
    ln -s "$T/b-after" "$T/b-after-aa"
    echo "run.sh --inner skips only what the outer run.sh does around it: the slot.sh wrap of each --no-run build and of --inner, and PERF_WAIT_QUIET. The build step runs the outer's own cargo command per side; --inner keeps its warm run per side, its ABBA plan and its --median count and commit asserts."
    stamp setup
    ;;
  build)
    name=$2
    crate=$3
    dir="$T/$name"
    packages=()
    while IFS= read -r word; do packages+=("$word"); done < <(packages_of "$dir")
    echo "build $name packages ${packages[*]}"
    stamp "build-$name-start"
    run_logged "build-$name" bash -c 'cd -P "$1" && shift && CARGO_TARGET_DIR="$PWD/.local-state/target" cargo test --release --locked "$@" --lib --no-run' _ "$dir" "${packages[@]}"
    gate "build-$name" "Compiling $crate v" "$T/logs/build-$name.log"
    stamp "build-$name-end"
    ;;
  drop)
    for name in "${@:2}"; do
      rm -rf "$T/$name/.local-state/target"
      note_rc "drop-$name" $?
    done
    stamp drop
    ;;
  load-start)
    stamp load-start
    (
      cd "$T/load" || exit 1
      export CARGO_TARGET_DIR="$T/load/.local-state/target"
      cargo fetch --locked > "$T/logs/load-fetch.log" 2>&1
      while :; do
        cargo build --workspace --exclude crazyagent-desktop --locked --offline >> "$T/logs/load.log" 2>&1
        cargo clean -p crazyagent -p crazyagent-kernel-processes >> "$T/logs/load.log" 2>&1
      done
    ) &
    echo $! > "$T/load.pid"
    sleep 90
    gate load-started "Compiling " "$T/logs/load.log"
    stamp load-running
    ;;
  load-stop)
    pid=$(cat "$T/load.pid")
    stamp load-stop
    pkill -TERM -P "$pid" || true
    kill -TERM "$pid" || true
    pkill -TERM -f "$T/load/.local-state/target" || true
    sleep 5
    pkill -KILL -f "$T/load/.local-state/target" || true
    kill -0 "$pid" 2> /dev/null && note_rc load-stopped 1 || note_rc load-stopped 0
    echo "load builds finished: $(grep -c '^    Finished' "$T/logs/load.log" || true)"
    stamp load-stopped
    ;;
  phase)
    reading=$2
    condition=$3
    labels=$4
    shift 4
    log="$T/logs/raw-$reading-$condition.log"
    : > "$log"
    free=$(free_gb)
    if [ "$free" -lt 10 ]; then
      echo "::error::$reading $condition: $free GB free, under 10"
      note_rc "phase-$reading-$condition" 1
      exit 0
    fi
    stamp "phase-$reading-$condition-start"
    rc=0
    PERF_LABELS="$labels" bash scripts/perf/run.sh --inner "$T/sprint" 4 "$log" "$@" > "$T/logs/median-$reading-$condition.log" 2>&1 || rc=$?
    cat "$T/logs/median-$reading-$condition.log"
    note_rc "phase-$reading-$condition" "$rc"
    stamp "phase-$reading-$condition-end"
    found=$(grep -c '^ *Compiling ' "$log" || true)
    echo "rebuilds inside the blocks: $found"
    if [ "$found" -eq 0 ]; then note_rc "same-binary-$reading-$condition" 0; else note_rc "same-binary-$reading-$condition" 1; fi
    echo "PERF-RUN lines: $(grep -c '^PERF-RUN ' "$log" || true)"
    ;;
  summary)
    case "$2" in
      A) specs=("A quiet 2.5 2.4.4" "A loaded 2.5 2.4.4") ;;
      B) specs=("B quiet 2.5 2.5-before" "B loaded 2.5 2.5-before") ;;
      *) echo "sitting.sh: summary takes A or B" >&2; exit 2 ;;
    esac
    for spec in "${specs[@]}"; do
      set -- $spec
      log="$T/logs/raw-$1-$2.log"
      if [ -s "$log" ]; then
        run_logged "summary-$1-$2" python3 -I scripts/perf/sitting_summary.py "$1" "$2" "$3" "$4" "$log"
      else
        echo "::error::no raw log for reading $1 $2"
        note_rc "summary-$1-$2" 1
      fi
    done
    grep -h '^VERDICT ' "$T"/logs/summary-*.log || true
    ;;
  verdict)
    bad=0
    for file in "$R"/*; do
      value=$(cat "$file")
      echo "rc $(basename "$file") $value"
      [ "$value" = 0 ] || bad=1
    done
    exit "$bad"
    ;;
  *)
    echo "sitting.sh: unknown step $1" >&2
    exit 2
    ;;
esac
