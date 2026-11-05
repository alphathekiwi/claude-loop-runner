#!/usr/bin/env bash
# Stand-in for the claude CLI in tests. Invoked as `fake-claude.sh -p <prompt> ...`
# from the task's working directory. Behaviour is driven by directive lines in
# the prompt (main or fixup), so each test scripts its own run:
#
#   FAKE_EXIT=<n>            exit with code n (default 0)
#   FAKE_APPEND_TARGET       append a line to the file named on the prompt's `File:` line
#   FAKE_WRITE=<path>        write "fake" to <path> (relative to the working dir)
#   FAKE_RESULT=<json>       print `RESULT: <json>` (default `RESULT: "done"`)
#   FAKE_LOG=<path>          append the target file path to <path>, one line per call
#   FAKE_STDERR=<text>       print <text> to stderr
#   FAKE_SLEEP=<secs>        sleep before handling the remaining directives
#   FAKE_BARRIER=<n>         drop a marker in .fake-barrier/ and wait (up to 15s) until n
#                            calls have done so; on timeout exit 7 before later directives
set -u

prompt=""
while [ $# -gt 0 ]; do
    case "$1" in
        -p) prompt="$2"; shift 2 ;;
        *) shift ;;
    esac
done

target=$(printf '%s\n' "$prompt" | sed -n 's/^File: //p' | head -n1)
exit_code=0
result='"done"'

while IFS= read -r line; do
    case "$line" in
        FAKE_EXIT=*) exit_code="${line#FAKE_EXIT=}" ;;
        FAKE_APPEND_TARGET) [ -n "$target" ] && echo "// edited by fake-claude" >> "$target" ;;
        FAKE_WRITE=*) path="${line#FAKE_WRITE=}"; mkdir -p "$(dirname "$path")"; echo fake > "$path" ;;
        FAKE_RESULT=*) result="${line#FAKE_RESULT=}" ;;
        FAKE_LOG=*) echo "$target" >> "${line#FAKE_LOG=}" ;;
        FAKE_STDERR=*) echo "${line#FAKE_STDERR=}" >&2 ;;
        FAKE_SLEEP=*) sleep "${line#FAKE_SLEEP=}" ;;
        FAKE_BARRIER=*)
            want="${line#FAKE_BARRIER=}"
            mkdir -p .fake-barrier
            : > ".fake-barrier/$$"
            waited=0
            while [ "$(ls .fake-barrier | wc -l)" -lt "$want" ]; do
                if [ "$waited" -ge 150 ]; then
                    echo "fake-claude: barrier timed out" >&2
                    exit 7
                fi
                sleep 0.1
                waited=$((waited + 1))
            done
            ;;
    esac
done <<< "$prompt"

echo "fake-claude processed ${target:-<no target>}"
echo "RESULT: $result"
exit "$exit_code"
