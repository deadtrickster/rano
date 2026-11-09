#!/bin/sh
# rano's send_command: the event arrives as JSON on stdin, and what this prints is the reply the
# sidebar shows. M-S and M-Q both call it; only M-Q (and the sidebar's Enter) carries an `ask`.
#
#   send_command = "/Users/dead/Projects/rano/rano/examples/ask.sh"
#
# Set RANO_ASK_MODEL to pick a model, or RANO_ASK_CMD to use another agent CLI - it gets the
# prompt on stdin and must print the answer.
set -eu

event=$(cat)
get() { printf '%s' "$event" | jq -r "$1 // empty"; }

file=$(get '.path')
line=$(get '.cursor.line')
ask=$(get '.ask')
selection=$(get '.selection.text')
modified=$(get '.modified')

if [ -z "$ask" ]; then
	# M-S without a question: say where the reader is, which is all it carries.
	printf 'at %s:%s\n' "${file:-the buffer}" "${line:-1}"
	exit 0
fi

prompt=$(printf 'In %s at line %s I am asking: %s\n' "${file:-an unsaved buffer}" "${line:-1}" "$ask")
if [ -n "$selection" ]; then
	prompt="$prompt
The selected text is:
<<<
$selection
>>>"
	if [ "$modified" = "true" ]; then
		prompt="$prompt
(The buffer has unsaved edits; the file on disk may differ from the selection.)"
	fi
fi
prompt="$prompt

Answer with the replacement code or the explanation only - no preamble, no restating the question."

if [ -n "${RANO_ASK_CMD:-}" ]; then
	# shellcheck disable=SC2086  # the operator's command line is theirs to split
	printf '%s' "$prompt" | $RANO_ASK_CMD
else
	printf '%s' "$prompt" | claude -p ${RANO_ASK_MODEL:+--model "$RANO_ASK_MODEL"}
fi
