# CVB control channel

CVB_CONTROL_FD=3
CVB_COMMAND_ID=0
CVB_COMMAND_ACTIVE=0
CVB_LAST_HISTCMD=
CVB_EXITING=0
CVB_SAVED_PROMPT_COMMAND=()
CVB_SAVED_PROMPT_IS_ARRAY=0

# History
#
# Keep the user's normal Bash history settings. The CVB session temporarily
# redirects HISTFILE to a private session file, then the parent shell imports
# the completed commands after the child exits.
#
# This function must be called as the final command in the generated rcfile.
# History stays disabled during all initialization because Bash records later
# lines of a sourced rcfile once `set -o history` is enabled.
cvb_control_history_start() {
    HISTFILE=$CVB_HISTORY_FILE
    history -c
    history -r "$CVB_PARENT_HISTORY_FILE"
    set -o history

    CVB_LAST_HISTCMD=$HISTCMD
}

cvb_control_write_byte() {
    printf '%b' "\\x$1" >&"$CVB_CONTROL_FD"
}

cvb_control_write_u32() {
    local value=$1

    printf '%b' "\\x${value:0:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\\x${value:2:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\\x${value:4:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\\x${value:6:2}" >&"$CVB_CONTROL_FD"
}

cvb_control_write_u64() {
    local value=$1

    printf '%b' "\\x${value:0:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\\x${value:2:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\\x${value:4:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\\x${value:6:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\\x${value:8:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\\x${value:10:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\\x${value:12:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\\x${value:14:2}" >&"$CVB_CONTROL_FD"
}

cvb_control_start() {
    local command=$1
    local copy=$2
    local command_length
    local payload_length

    ((CVB_COMMAND_ID++))
    CVB_COMMAND_ACTIVE=1

    command_length=$(printf '%s' "$command" | wc -c)
    payload_length=$(printf '%08x' $((10 + command_length)))

    cvb_control_write_u32 "$payload_length"
    cvb_control_write_byte 01
    cvb_control_write_byte "$copy"
    cvb_control_write_u64 "$(printf '%016x' "$CVB_COMMAND_ID")"
    printf '%s' "$command" >&"$CVB_CONTROL_FD"
}

cvb_control_end() {
    local status=$1

    cvb_control_write_u32 0000000d
    cvb_control_write_byte 02
    cvb_control_write_u64 "$(printf '%016x' "$CVB_COMMAND_ID")"
    cvb_control_write_u32 "$(printf '%08x' "$status")"

    printf '\e]777;CVB;END\a'
}

cvb_control_exit() {
    CVB_EXITING=1
    history -d "$HISTCMD"
    set +o history

    cvb_control_write_u32 00000001
    cvb_control_write_byte 03
}

cvb_control_prompt() {
    local status=$?
    local command
    local copy=1

    if ((CVB_EXITING)); then
        return
    fi

    history -a

    if ((HISTCMD != CVB_LAST_HISTCMD)); then
        command=$(fc -ln -1)

        if [[ $command != exit && $command != exit[[:space:]]* ]]; then
            CVB_LAST_HISTCMD=$HISTCMD

            [[ $command == [[:space:]]* ]] && copy=0

            cvb_control_start "$command" "$copy"
        fi
    fi

    if ((CVB_SAVED_PROMPT_IS_ARRAY)); then
        for command in "${CVB_SAVED_PROMPT_COMMAND[@]}"; do
            eval "$command"
        done
    elif ((${#CVB_SAVED_PROMPT_COMMAND[@]})); then
        eval "${CVB_SAVED_PROMPT_COMMAND[0]}"
    fi

    ((CVB_EXITING)) && return

    cvb_control_end "$status"
}

if declare -p PROMPT_COMMAND &>/dev/null; then
    if [[ $(declare -p PROMPT_COMMAND) == "declare -a"* ]]; then
        CVB_SAVED_PROMPT_COMMAND=("${PROMPT_COMMAND[@]}")
        CVB_SAVED_PROMPT_IS_ARRAY=1
    else
        CVB_SAVED_PROMPT_COMMAND=("$PROMPT_COMMAND")
    fi
fi

PROMPT_COMMAND='cvb_control_prompt'

CVB_SAVED_PS1=$PS1
PS1='\[\e[31m\]●\[\e[0m\] '"$CVB_SAVED_PS1"
