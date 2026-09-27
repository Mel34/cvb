# CVB control channel

CVB_CONTROL_FD=3
CVB_COMMAND_ID=0
CVB_COMMAND_ACTIVE=0
CVB_LAST_HISTCMD=
CVB_EXITING=0
CVB_SAVED_PROMPT_COMMAND=()
CVB_SAVED_PROMPT_IS_ARRAY=0

# CVB owns command history inside the child shell.
HISTSIZE=1000
HISTFILESIZE=1000
HISTCONTROL=
HISTIGNORE=
HISTTIMEFORMAT=
HISTFILE=/dev/null

# IMPORTANT: history is enabled by the generated rcfile only after
# CVB bootstrap and the parent history transaction boundary are ready.

shopt -s cmdhist
shopt -s lithist

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
    local payload_length

    ((CVB_COMMAND_ID++))
    CVB_COMMAND_ACTIVE=1

    payload_length=$(printf '%08x' $((10 + ${#command})))

    cvb_control_write_u32 "$payload_length"
    cvb_control_write_byte 01
    cvb_control_write_byte "$copy"
    cvb_control_write_u64 "$(printf '%016x' "$CVB_COMMAND_ID")"
    printf '%s' "$command" >&"$CVB_CONTROL_FD"
}

cvb_control_end() {
    local status=$1
    local status_hex

    ((CVB_COMMAND_ACTIVE)) || return

    status_hex=$(printf '%08x' "$status")

    cvb_control_write_u32 0000000d
    cvb_control_write_byte 02
    cvb_control_write_u64 "$(printf '%016x' "$CVB_COMMAND_ID")"
    cvb_control_write_u32 "$status_hex"

    CVB_COMMAND_ACTIVE=0
}

cvb_control_exit() {
    CVB_EXITING=1
    set +o history

    cvb_control_write_u32 00000001
    cvb_control_write_byte 03
}

cvb_control_debug() {
    ((CVB_EXITING)) && return
    local history_line
    local command
    local normalized_command
    local copy=1

    ((CVB_COMMAND_ACTIVE)) && return
    [[ ${BASH_COMMAND} == cvb_control_* ]] && return
    ((HISTCMD > 0)) || return
    ((HISTCMD == CVB_LAST_HISTCMD)) && return

    history_line=$(history 1)
    history_line="${history_line#"${history_line%%[![:space:]]*}"}"
    command="${history_line#*[[:space:]]}"
    normalized_command="${command#"${command%%[![:space:]]*}"}"

    [[ -n $normalized_command ]] || return

    [[ ${history_line} =~ ^[0-9]+[[:space:]]{3} ]] && copy=0

    [[ $normalized_command == exit || $normalized_command == exit[[:space:]]* ]] && return
    [[ $normalized_command == cvb || $normalized_command == cvb[[:space:]]* ]] && return

    CVB_LAST_HISTCMD=$HISTCMD

    printf '%s\n' "$command" >>"$CVB_HISTORY_FILE"

    cvb_control_start "$command" "$copy"
}

cvb_control_prompt() {
    local status=$?

    if ((CVB_SAVED_PROMPT_IS_ARRAY)); then
        local command
        for command in "${CVB_SAVED_PROMPT_COMMAND[@]}"; do
            eval "$command"
        done
    elif ((${#CVB_SAVED_PROMPT_COMMAND[@]})); then
        eval "${CVB_SAVED_PROMPT_COMMAND[0]}"
    fi

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
trap 'cvb_control_debug' DEBUG

CVB_SAVED_PS1=$PS1
PS1='\[\e[31m\]●\[\e[0m\] '"$CVB_SAVED_PS1"