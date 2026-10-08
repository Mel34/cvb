CVB_START_MARKER=$'\e]777;CVB;START\a'
CVB_END_MARKER=$'\e]777;CVB;END\a'

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
cvb_control_history_debug() {
    local command=$BASH_COMMAND

    if [[ $command == exit || $command == exit[[:space:]]* ]]; then
        history -d "$HISTCMD"
        set +o history
    fi
}

cvb_control_history_start() {
    HISTFILE=$CVB_HISTORY_FILE
    history -c
    history -r "$CVB_PARENT_HISTORY_FILE"
    set -o history
    trap cvb_control_history_debug DEBUG
}

cvb_control_restore_status() {
    return "$1"
}

cvb_control_prompt() {
    local status=$?
    local command

    history -a

    printf '%s' "$CVB_END_MARKER"

    if ((CVB_SAVED_PROMPT_IS_ARRAY)); then
        for command in "${CVB_SAVED_PROMPT_COMMAND[@]}"; do
            cvb_control_restore_status "$status"
            eval "$command"
        done
    elif ((${#CVB_SAVED_PROMPT_COMMAND[@]})); then
        cvb_control_restore_status "$status"
        eval "${CVB_SAVED_PROMPT_COMMAND[0]}"
    fi
}

if declare -p PROMPT_COMMAND &>/dev/null; then
    if [[ $(declare -p PROMPT_COMMAND) == "declare -a"* ]]; then
        CVB_SAVED_PROMPT_COMMAND=("${PROMPT_COMMAND[@]}")
        CVB_SAVED_PROMPT_IS_ARRAY=1
    elif [[ -n ${PROMPT_COMMAND-} ]]; then
        CVB_SAVED_PROMPT_COMMAND=("$PROMPT_COMMAND")
    fi
fi

PS0=$CVB_START_MARKER
PROMPT_COMMAND=cvb_control_prompt

CVB_SAVED_PS1=$PS1
PS1='\[\e[31m\]●\[\e[0m\] '"$CVB_SAVED_PS1"

cvb() {
    if [[ -z "$1" ]]; then
        printf 'CVB: already running\n' >&2
        return 1
    fi

    local path
    for path in "$@"; do
        if [[ ! -e $path ]]; then
            command cvb "$@"
            return $?
        fi
    done

    CVB_INTERNAL_TARGET=1 command cvb "$@"
}
