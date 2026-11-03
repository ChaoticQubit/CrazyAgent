input=$(head -c 65536)
cat >/dev/null
[ -f graphify-out/graph.json ] || exit 0
case "$1" in
search)
  case "$input" in
    *grep*|*ripgrep*|*'rg '*|*'find '*|*'fd '*|*'ack '*|*'ag '*) ;;
    *) exit 0 ;;
  esac
  text='graphify-out/graph.json exists. Run graphify query with your question before grepping raw files; grep only after graphify has oriented you, or to change specific lines.'
  ;;
read)
  case "$input" in
    *graphify-out/*) exit 0 ;;
    *'.py"'*|*'.js"'*|*'.ts"'*|*'.tsx"'*|*'.jsx"'*|*'.go"'*|*'.rs"'*|*'.java"'*|*'.rb"'*|*'.c"'*|*'.h"'*|*'.cpp"'*|*'.hpp"'*|*'.cc"'*|*'.cs"'*|*'.kt"'*|*'.swift"'*|*'.php"'*|*'.scala"'*|*'.lua"'*|*'.sh"'*|*'.md"'*|*'.mdx"'*|*'.rst"'*|*'.txt"'*|*'.vue"'*|*'.svelte"'*|*'.astro"'*) ;;
    *) exit 0 ;;
  esac
  text='graphify-out/graph.json exists. Run graphify query, path or explain before reading raw source files; read a file only after graphify has oriented you, or to change specific lines. Give every sub-agent and teammate the same rule.'
  ;;
*)
  exit 0
  ;;
esac
printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"%s"}}\n' "$text"
