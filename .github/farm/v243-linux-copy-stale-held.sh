#!/usr/bin/env bash
set -euo pipefail
fix=6ef1ea3d5cadc6b6f6a89b1f207e5e93c25e0d5f
red=f2fb97b38502b3a8b47c2a2115193d169551d68b
row="off a Mac, a held Shift press whose release never reached the page is dropped"
files="apps/desktop/test/terminal-copy.test.mjs apps/desktop/test/terminal-wall.test.mjs apps/desktop/test/kept-views.test.mjs apps/desktop/test/team-graph.test.mjs apps/desktop/test/terminal-renderer.test.mjs apps/desktop/test/live-master.test.mjs apps/desktop/test/reinstall-offer.test.mjs"
git -c advice.detachedHead=false checkout -q "$fix"
echo "=== GREEN at fix $fix: the panel's node test files"
set +e
node --test --test-timeout=15000 --test-reporter=spec $files 2>&1 | tee "$RUNNER_TEMP/green.log"
green=${PIPESTATUS[0]}
set -e
echo "GREEN rc=$green"
git -c advice.detachedHead=false checkout -q "$red"
echo "=== RED at $red: terminal-copy.test.mjs, the new row must fail"
set +e
node --test --test-timeout=15000 --test-reporter=spec apps/desktop/test/terminal-copy.test.mjs 2>&1 | tee "$RUNNER_TEMP/red.log"
redrc=${PIPESTATUS[0]}
set -e
echo "RED rc=$redrc"
grep -F "✖ $row" "$RUNNER_TEMP/red.log" >/dev/null && redrow=failed || redrow=passed
echo "RED new row: $redrow"
[ "$green" -eq 0 ] && [ "$redrc" -ne 0 ] && [ "$redrow" = failed ]
