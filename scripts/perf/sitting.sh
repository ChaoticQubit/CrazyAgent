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
    for pair in b-after=2735d10968 b-before=c6f1398838 load=008adcd803; do
      name=${pair%%=*}
      rev=${pair#*=}
      rc=0
      git worktree add -q --detach "$T/$name" "$rev" || rc=$?
      note_rc "worktree-$name" "$rc"
      echo "side $name commit $(git -C "$T/$name" rev-parse --short=10 HEAD)"
    done
    ln -s "$T/b-after" "$T/b-after-aa"
    echo "run.sh --inner skips only what the outer run.sh does around it: the slot.sh wrap of each --no-run build and of --inner, and PERF_WAIT_QUIET. The build step runs the outer's own cargo command per side; --inner keeps its warm run per side, its ABBA plan and its --median count and commit asserts."
    stamp setup
    ;;
  build)
    name=$2
    crate=$3
    dir="$T/$name"
    packages=()
    if [ -n "${PERF_PACKAGES:-}" ]; then
      for word in $PERF_PACKAGES; do packages+=(-p "$word"); done
    else
      while IFS= read -r word; do packages+=("$word"); done < <(packages_of "$dir")
    fi
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
    groups="$T/phase-$reading-$condition.groups"
    alive() {
      local group
      for group in $(cat "$@" 2> /dev/null); do
        kill -0 -- "-$group" 2> /dev/null && return 0
      done
      return 1
    }
    for earlier in "$T"/phase-*.groups; do
      [ -e "$earlier" ] || continue
      if alive "$earlier"; then
        echo "::error::$reading $condition: the phase of $(basename "$earlier") still runs"
        note_rc "phase-$reading-$condition" 1
        exit 0
      fi
    done
    : > "$groups"
    stamp "phase-$reading-$condition-start"
    rc=0
    PERF_PGID_FILE="$groups" PERF_LABELS="$labels" python3 -I -c 'import os, sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])' \
      bash scripts/perf/run.sh --inner "$T/sprint" 4 "$log" "$@" > "$T/logs/median-$reading-$condition.log" &
    lead=$!
    echo "$lead" >> "$groups"
    stop_phase() {
      local group
      for group in $(cat "$groups"); do kill -TERM -- "-$group" 2> /dev/null || true; done
      alive "$groups" && sleep 5
      for group in $(cat "$groups"); do kill -KILL -- "-$group" 2> /dev/null || true; done
      return 0
    }
    trap 'echo "::error::$reading $condition: stopped by a signal"; stop_phase; note_rc "phase-$reading-$condition" 143; exit 143' TERM INT HUP
    watch=""
    if [ "${PHASE_SECS:-0}" -gt 0 ]; then
      (
        trap 'kill "$nap" 2> /dev/null; exit 0' TERM
        sleep "$PHASE_SECS" &
        nap=$!
        wait "$nap"
        echo "::error::$reading $condition: past its deadline of ${PHASE_SECS}s"
        for group in $(cat "$groups"); do kill -TERM -- "-$group" 2> /dev/null; done
      ) &
      watch=$!
    fi
    wait "$lead" || rc=$?
    if [ -n "$watch" ]; then
      kill "$watch" 2> /dev/null || true
      wait "$watch" 2> /dev/null || true
    fi
    trap - TERM INT HUP
    stop_phase
    if alive "$groups"; then
      echo "::error::$reading $condition: a process of the phase outlived it"
      rc=1
    fi
    echo "phase groups: $(wc -l < "$groups" | tr -d ' ')"
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
        if [ -f "$RUNNER_TEMP/ratios.py" ]; then
          run_logged "ratios-$1-$2" python3 -I "$RUNNER_TEMP/ratios.py" "$log" --pair "$3,$4" --aa 2.5-aa
        fi
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
