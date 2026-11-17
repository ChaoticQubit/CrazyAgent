set -euo pipefail
version=${NEXTEST_VERSION:?NEXTEST_VERSION is not set}
case "${RUNNER_OS:-}" in
  Linux) asset="cargo-nextest-$version-x86_64-unknown-linux-gnu.tar.gz" sha=d039b43c3e0958441b6ad0a39c8884d018b3d2e8fb9928f209074b1100b5e17a ;;
  macOS) asset="cargo-nextest-$version-universal-apple-darwin.tar.gz" sha=2fadf57df4243e691f088ee3bff4f4ab9af16da0c95a1db84b5cb1685e098a76 ;;
  Windows)
    case "${RUNNER_ARCH:-}" in
      ARM64) asset="cargo-nextest-$version-aarch64-pc-windows-msvc.zip" sha=89a3a53f81da769d9e67bee6c537bee2be668d2dec4084f3cb0dcfb615b063e8 ;;
      *) asset="cargo-nextest-$version-x86_64-pc-windows-msvc.zip" sha=cc6bf4b6a7ac18e462cd6c9bae67e26921b888249c2988c6acabf3df387ed9d6 ;;
    esac ;;
  *) echo "::error::no pinned cargo-nextest for RUNNER_OS=${RUNNER_OS:-unset}"; exit 1 ;;
esac
[ "$version" = 0.9.152 ] || { echo "::error::the sha256 pins are for cargo-nextest 0.9.152, not $version"; exit 1; }
work="${RUNNER_TEMP:?RUNNER_TEMP is not set}/nextest"
mkdir -p "$work/bin"
curl -fsSL --proto '=https' --tlsv1.2 -o "$work/$asset" "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$version/$asset"
if command -v sha256sum >/dev/null; then echo "$sha  $work/$asset" | sha256sum -c -; else echo "$sha  $work/$asset" | shasum -a 256 -c -; fi
case "$asset" in
  *.zip) if command -v unzip >/dev/null; then unzip -q -o "$work/$asset" -d "$work/bin"; else 7z x -y -o"$work/bin" "$work/$asset" >/dev/null; fi ;;
  *) tar -xzf "$work/$asset" -C "$work/bin" ;;
esac
echo "$work/bin" >> "${GITHUB_PATH:?GITHUB_PATH is not set}"
"$work/bin/cargo-nextest" nextest --version | grep -F "cargo-nextest $version"
