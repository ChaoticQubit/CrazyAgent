#!/bin/sh
set -u
environment=$(env)
PATH=/usr/bin:/bin
LC_ALL=C
export PATH LC_ALL
umask 077

refuse() {
	printf 'standin: %s\n' "$1" >&2
	exit 2
}

safe_name() {
	case $1 in
	'' | .* | *[!A-Za-z0-9_.-]*) return 1 ;;
	esac
	[ ${#1} -le 64 ]
}

kind=${0##*/}
kind=${kind#standin-}
home=${STANDIN_HOME:-}
session=
prompt=
has_prompt=0
record=
read_stdin=0
stopped=
want=
for arg in "$@"; do
	if [ -n "$want" ]; then
		case $want in
		kind) kind=$arg ;;
		home) home=$arg ;;
		session | resume) session=$arg ;;
		prompt)
			prompt=$arg
			has_prompt=1
			;;
		record) record=$arg ;;
		esac
		want=
		continue
	fi
	case $arg in
	--kind) want=kind ;;
	--home) want=home ;;
	--session) want=session ;;
	--resume) want=resume ;;
	--prompt) want=prompt ;;
	--prompt=*)
		prompt=${arg#--prompt=}
		has_prompt=1
		;;
	--record) want=record ;;
	--stdin) read_stdin=1 ;;
	esac
done
[ -z "$want" ] || refuse "--$want needs a value."

case $kind in
acp | jsonl-turn | jsonl-rpc | text-turn) ;;
jsonl-append | json-rewrite | sqlite-poll | markdown-history | screen) ;;
*) refuse "the program name or --kind names no stand-in kind." ;;
esac
case $home in
/*) ;;
*) refuse "--home or STANDIN_HOME must name an absolute folder." ;;
esac
session_refusal="the session id is not 1 to 64 letters, digits, dots, dashes or underscores."
if [ -n "$session" ] && ! safe_name "$session"; then
	refuse "$session_refusal"
fi
case $record in
'' | /*) ;;
*) refuse "--record must name an absolute file." ;;
esac
state=$home/standin
mkdir -p "$state" || refuse "the folder $state cannot be made."

count() {
	counted=0
	if [ -f "$state/$1" ]; then
		counted=$(cat "$state/$1")
	fi
	case $counted in
	'' | *[!0-9]*) refuse "the counter $1 is not a number." ;;
	esac
	counted=$((counted + 1))
	printf '%s\n' "$counted" >"$state/$1"
}

count runs
run=$counted
if [ $# -gt 0 ]; then
	printf '%s\000' "$@" >"$state/argv-$run"
else
	: >"$state/argv-$run"
fi
printf '%s\n' "$environment" >"$state/env-$run"
pwd -P >"$state/cwd-$run"
if [ "$has_prompt" = 1 ]; then
	printf '%s' "$prompt" >"$state/prompt-$run"
fi
if [ -z "$session" ] && [ -f "$state/session-id" ]; then
	session=$(cat "$state/session-id")
fi
[ -n "$session" ] || session=standin-$run
safe_name "$session" || refuse "$session_refusal"
printf '%s\n' "$session" >"$state/session-$run"
if [ "$read_stdin" = 1 ]; then
	cat >>"$state/stdin-$run"
fi

next_turn() {
	count turns
	turn=$counted
}

fixture() {
	if [ -f "$state/turn-$turn.$1" ]; then
		printf '%s' "$state/turn-$turn.$1"
	elif [ -f "$state/turn.$1" ]; then
		printf '%s' "$state/turn.$1"
	fi
}

one_line() {
	tr -d '\r\n' <"$1"
}

interrupt_line() {
	[ -f "$state/interrupt.match" ] || return 1
	match=$(cat "$state/interrupt.match")
	[ -n "$match" ] || return 1
	case $1 in
	*"$match"*) return 0 ;;
	esac
	return 1
}

parse_head() {
	head=${1%%\"params\"*}
	head=${head%%\"result\"*}
	head=${head%%\"error\"*}
	method=$(printf '%s\n' "$head" |
		sed -n -E 's/.*"method"[[:space:]]*:[[:space:]]*"([A-Za-z0-9_/.-]+)".*/\1/p')
	id=$(printf '%s\n' "$head" |
		sed -n -E 's/.*"id"[[:space:]]*:[[:space:]]*("[A-Za-z0-9_.:-]*"|-?[0-9]+).*/\1/p')
}

heard() {
	waited=
	if ! IFS= read -r waited; then
		if [ -z "$waited" ]; then
			stopped=eof
			return 0
		fi
	fi
	printf '%s\n' "$waited" >>"$state/stdin-$run"
	case $kind in
	acp)
		parse_head "$waited"
		if [ "$method" = session/cancel ]; then
			stopped=cancelled
		fi
		;;
	*)
		if interrupt_line "$waited"; then
			stopped=interrupted
		fi
		;;
	esac
}

replay() {
	stopped=
	[ -n "$1" ] || return 0
	if ! grep -q '^@standin ' "$1"; then
		cat "$1"
		return 0
	fi
	while :; do
		if [ -n "${interrupted:-}" ]; then
			stopped=interrupted
			break
		fi
		line=
		ended=
		IFS= read -r line <&3 || ended=1
		if [ -n "$ended" ] && [ -z "$line" ]; then
			break
		fi
		case $line in
		'@standin wait')
			heard
			;;
		'@standin exit '*)
			code=${line#'@standin exit '}
			case $code in
			'' | *[!0-9]*) refuse "an exit directive needs a number." ;;
			esac
			exit "$code"
			;;
		'@standin sleep '*)
			pause=${line#'@standin sleep '}
			case $pause in
			'' | *[!0-9.]*) refuse "a sleep directive needs a number of seconds." ;;
			esac
			sleep "$pause"
			;;
		*)
			if [ -n "$ended" ]; then
				printf '%s' "$line"
			else
				printf '%s\n' "$line"
			fi
			;;
		esac
		[ -z "$stopped" ] || break
		[ -z "$ended" ] || break
	done 3<"$1"
}

interrupt_out() {
	if [ -f "$state/interrupt.out" ]; then
		cat "$state/interrupt.out"
	fi
}

reply() {
	printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$1" "$2"
}

stop_reason() {
	reason=end_turn
	file=$(fixture stop)
	if [ -n "$file" ]; then
		reason=$(one_line "$file")
	fi
	case $reason in
	'' | *[!a-z_]*) refuse "turn-$turn.stop is not a stop reason." ;;
	esac
}

serve_once() {
	next_turn
	replay "$(fixture out)"
	file=$(fixture err)
	if [ -n "$file" ]; then
		cat "$file" >&2
	fi
	code=0
	file=$(fixture exit)
	if [ -n "$file" ]; then
		code=$(tr -d ' \r\n' <"$file")
	fi
	case $code in
	'' | *[!0-9]*) refuse "turn-$turn.exit is not a number." ;;
	esac
	[ "$code" -le 255 ] || refuse "turn-$turn.exit is above 255."
	exit "$code"
}

serve_rpc() {
	if [ -f "$state/ready.out" ]; then
		cat "$state/ready.out"
	fi
	while IFS= read -r incoming || [ -n "$incoming" ]; do
		printf '%s\n' "$incoming" >>"$state/stdin-$run"
		if interrupt_line "$incoming"; then
			interrupt_out
			continue
		fi
		next_turn
		replay "$(fixture out)"
		if [ "$stopped" = interrupted ]; then
			interrupt_out
		fi
		[ "$stopped" != eof ] || break
	done
	exit 0
}

serve_acp() {
	while IFS= read -r incoming || [ -n "$incoming" ]; do
		printf '%s\n' "$incoming" >>"$state/stdin-$run"
		parse_head "$incoming"
		request=$id
		[ -n "$method" ] || continue
		slug=$(printf '%s' "$method" | tr '/' '-')
		safe_name "$slug" || continue
		if [ -f "$state/acp-$slug.out" ]; then
			replay "$state/acp-$slug.out"
			[ "$stopped" != eof ] || break
		fi
		[ -n "$request" ] || continue
		if [ -f "$state/acp-$slug.error" ]; then
			printf '{"jsonrpc":"2.0","id":%s,"error":%s}\n' "$request" \
				"$(one_line "$state/acp-$slug.error")"
			continue
		fi
		if [ "$method" = session/prompt ]; then
			next_turn
			replay "$(fixture out)"
			[ "$stopped" != eof ] || break
			if [ "$stopped" = cancelled ]; then
				reason=cancelled
			else
				stop_reason
			fi
			reply "$request" "{\"stopReason\":\"$reason\"}"
			continue
		fi
		if [ -f "$state/acp-$slug.json" ]; then
			reply "$request" "$(one_line "$state/acp-$slug.json")"
			continue
		fi
		case $method in
		initialize)
			reply "$request" \
				'{"protocolVersion":1,"agentCapabilities":{"loadSession":true},"authMethods":[]}'
			;;
		session/new) reply "$request" "{\"sessionId\":\"$session\"}" ;;
		session/load | session/resume | session/close) reply "$request" '{}' ;;
		*)
			printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' \
				"$request"
			;;
		esac
	done
	exit 0
}

write_record() {
	case $kind in
	jsonl-append) piece=$(fixture jsonl) ;;
	json-rewrite) piece=$(fixture json) ;;
	sqlite-poll) piece=$(fixture sql) ;;
	markdown-history) piece=$(fixture md) ;;
	*) piece= ;;
	esac
	[ -n "$piece" ] || return 0
	case $kind in
	jsonl-append | markdown-history) cat "$piece" >>"$record" ;;
	json-rewrite)
		cat "$piece" >"$record.standin-tmp"
		mv -f "$record.standin-tmp" "$record"
		;;
	sqlite-poll)
		sqlite3 -bail "$record" <"$piece" || refuse "sqlite3 could not run turn $turn's SQL."
		;;
	esac
}

prompt_mark() {
	if [ -f "$state/prompt-mark" ]; then
		cat "$state/prompt-mark"
	else
		printf '> '
	fi
}

serve_terminal() {
	case $kind in
	jsonl-append) ending=jsonl ;;
	json-rewrite) ending=json ;;
	sqlite-poll) ending=db ;;
	markdown-history) ending=md ;;
	*) ending= ;;
	esac
	if [ -z "$record" ] && [ -n "$ending" ]; then
		record=$state/records/$session.$ending
	fi
	if [ -n "$record" ]; then
		mkdir -p "${record%/*}" || refuse "the record folder cannot be made."
	fi
	if [ "$kind" = sqlite-poll ] && ! command -v sqlite3 >/dev/null 2>&1; then
		refuse "sqlite3 is not in /usr/bin or /bin, so the sqlite-poll record cannot be written."
	fi
	interrupted=
	trap 'interrupted=1' INT
	if [ -f "$state/first-screen.out" ]; then
		replay "$state/first-screen.out"
	fi
	prompt_mark
	while :; do
		typed=
		if ! IFS= read -r typed; then
			if [ -n "$interrupted" ]; then
				interrupted=
				continue
			fi
			[ -n "$typed" ] || break
		fi
		printf '%s\n' "$typed" >>"$state/stdin-$run"
		next_turn
		interrupted=
		replay "$(fixture out)"
		if [ "$stopped" = interrupted ] || [ -n "$interrupted" ]; then
			interrupted=
			interrupt_out
		fi
		write_record
		[ "$stopped" != eof ] || break
		prompt_mark
	done
	exit 0
}

case $kind in
acp) serve_acp ;;
jsonl-rpc) serve_rpc ;;
jsonl-turn | text-turn) serve_once ;;
*) serve_terminal ;;
esac
