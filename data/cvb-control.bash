CVB_START_MARKER=$'\e]777;CVB;START\a'
CVB_END_MARKER_PREFIX=$'\e]777;CVB;END;'
CVB_SKIP_MARKER=$'\e]777;CVB;SKIP\a'
CVB_COMMAND_MARKER_PREFIX=$'\e]777;CVB;COMMAND;'

# Top-level simple commands run since the last prompt, recorded by the DEBUG
# trap. Used to decide whether an empty capture may be skipped (see
# cvb_control_skip_listed).
CVB_TRACED_COMMANDS=()

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
cvb_control_debug() {
    local command=$BASH_COMMAND

    if [[ $command == exit || $command == exit[[:space:]]* ]]; then
        history -d "$HISTCMD"
        set +o history
    fi

    # Record only top-level commands. FUNCNAME holds just this trap function
    # unless `functrace` is on and the command runs inside another function.
    # The prompt hook itself is not part of the user's command line.
    if ((${#FUNCNAME[@]} == 1)) && [[ $command != cvb_control_prompt ]]; then
        CVB_TRACED_COMMANDS+=("$command")
        cvb_control_emit_command "$command"
    fi
}

cvb_control_history_start() {
    HISTFILE=$CVB_HISTORY_FILE
    history -c
    history -r "$CVB_PARENT_HISTORY_FILE"
    set -o history
    trap cvb_control_debug DEBUG
}

# Skip list
#
# CVB_SKIP_FILE (exported by CVB, normally ~/.config/cvb/skip) lists command
# names, one per line; blank lines and lines starting with # are ignored. The
# file is read at every check, so edits apply immediately.
#
# Succeeds when every top-level command of the finished command line is
# listed, comparing the first word of each command. The Rust side still
# decides whether the capture is actually skipped: it does so only when the
# command line also produced no visible output.
cvb_control_skip_listed() {
    local file=${CVB_SKIP_FILE-}
    local line word command
    local listed=$'\n'

    ((${#CVB_TRACED_COMMANDS[@]})) || return 1
    [[ -n $file && -r $file ]] || return 1

    while IFS= read -r line || [[ -n $line ]]; do
        line=${line%%#*}
        word=
        read -r word _ <<<"$line"
        [[ -n $word ]] && listed+=$word$'\n'
    done <"$file"

    for command in "${CVB_TRACED_COMMANDS[@]}"; do
        word=
        read -r word _ <<<"$command"
        [[ -n $word && $listed == *$'\n'"$word"$'\n'* ]] || return 1
    done

    return 0
}

cvb_control_emit_command() {
    local command=$1 byte hex index
    local LC_CTYPE=C

    hex=
    for ((index = 0; index < ${#command}; index++)); do
        printf -v byte '%02x' "'${command:index:1}"
        hex+=$byte
    done

    printf '%s%s\a' "$CVB_COMMAND_MARKER_PREFIX" "$hex"
}

cvb_control_restore_status() {
    return "$1"
}

cvb_control_prompt() {
    local status=$?
    local command

    history -a

    if cvb_control_skip_listed; then
        printf '%s' "$CVB_SKIP_MARKER"
    fi

    CVB_TRACED_COMMANDS=()

    printf '%s%03d\a' "$CVB_END_MARKER_PREFIX" "$status"

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

printf '\nCVB context initialization ready.\nPaste into your LLM to begin.\n\n'

CVB_SAVED_PS1=$PS1
PS1='\[\e[31m\]●\[\e[0m\] '"$CVB_SAVED_PS1"

# Every line the shell reads from a pasted command block ends either in a
# completed prompt (PROMPT_COMMAND prints the END marker) or in a continuation
# prompt. The continuation prompt prints this marker so CVB can tell that the
# line was read but the command is not finished yet (an if block, a heredoc, a
# trailing backslash, an open quote).
PS2='\[\e]777;CVB;CONT\a\]'"${PS2-> }"

# Tab completion for cvb. The functions come from the parent shell with the
# other shell state; only the registration is missing in this shell.
declare -F _cvb_complete >/dev/null 2>&1 && complete -F _cvb_complete cvb

# Paste-to-apply: after CVB prints the result of a pasted patch it sends this
# private key sequence, which redraws the prompt and anything half-typed without
# running a command or touching history. Bound in every editing mode.
bind -m emacs '"\e[9999~": redraw-current-line' 2>/dev/null
bind -m vi-insert '"\e[9999~": redraw-current-line' 2>/dev/null
bind -m vi-command '"\e[9999~": redraw-current-line' 2>/dev/null

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
