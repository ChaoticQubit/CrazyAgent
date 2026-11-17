set -euo pipefail
claude_version=${CLAUDE_CODE_VERSION:?CLAUDE_CODE_VERSION is not set}
codex_version=${CODEX_VERSION:?CODEX_VERSION is not set}
case "${RUNNER_OS:-}" in
  Linux)
    claude_package=claude-code-linux-x64 claude_sha=e960c4437f6f99648437784db43e04ad5f12711aecd710864f96876f94c2fc1a
    codex_package=codex-package-x86_64-unknown-linux-musl.tar.gz codex_sha=3ec55053109608d5038f7e50750f7d25f768dfd740fef071aa710e2559b63aac
    ;;
  macOS)
    claude_package=claude-code-darwin-arm64 claude_sha=0e8ce1ddb8566129661bb7698db69db89d904e23b464afc6e85d5bd8879b9a51
    codex_package=codex-package-aarch64-apple-darwin.tar.gz codex_sha=dd99e9092270691a67bdde7be04ac283dea9da8e86cbde356c2cc851a79205fa
    ;;
  *) echo "::error::no pinned Claude Code or Codex for RUNNER_OS=${RUNNER_OS:-unset}"; exit 1 ;;
esac
[ "$claude_version/$codex_version" = 2.1.309/0.164.0 ] || { echo "::error::the sha256 pins are for Claude Code 2.1.309 and Codex 0.164.0, not $claude_version and $codex_version"; exit 1; }
work="${RUNNER_TEMP:?RUNNER_TEMP is not set}/harnesses"
mkdir -p "$work/claude/bin" "$work/codex"
curl -fsSL --proto '=https' --tlsv1.2 -o "$work/claude.tgz" "https://registry.npmjs.org/@anthropic-ai/$claude_package/-/$claude_package-$claude_version.tgz"
echo "$claude_sha  $work/claude.tgz" | shasum -a 256 -c -
tar -xzf "$work/claude.tgz" -C "$work/claude" package/claude
install -m 0755 "$work/claude/package/claude" "$work/claude/bin/claude"
test "$("$work/claude/bin/claude" --version | cut -d' ' -f1)" = "$claude_version"
curl -fsSL --proto '=https' --tlsv1.2 -o "$work/codex.tgz" "https://github.com/openai/codex/releases/download/rust-v$codex_version/$codex_package"
echo "$codex_sha  $work/codex.tgz" | shasum -a 256 -c -
tar -xzf "$work/codex.tgz" -C "$work/codex"
test "$("$work/codex/bin/codex" --version)" = "codex-cli $codex_version"
{
  echo "$work/claude/bin"
  echo "$work/codex/bin"
} >> "${GITHUB_PATH:?GITHUB_PATH is not set}"
{
  echo "CRAZYAGENT_CLAUDE=$work/claude/bin/claude"
  echo "CRAZYAGENT_CODEX=$work/codex/bin/codex"
} >> "${GITHUB_ENV:?GITHUB_ENV is not set}"
