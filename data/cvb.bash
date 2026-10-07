cvb() {
    if [[ -z "$1" ]]; then
        if [[ -n ${CVB_ACTIVE:-} ]]; then
            printf 'CVB: already running\n' >&2
            return 1
        fi

        local runtime_dir="${XDG_RUNTIME_DIR:-/tmp}/cvb"
        local init_file
        local history_file
        local parent_history_file
        local previous_cvb_active=${CVB_ACTIVE+x}
        local previous_cvb_active_value=${CVB_ACTIVE-}
        local previous_cvb_history_file=${CVB_HISTORY_FILE-}
        local previous_cvb_marker_key=${CVB_MARKER_KEY+x}
        local previous_cvb_marker_key_value=${CVB_MARKER_KEY-}

        mkdir -p "$runtime_dir" || return 1

        init_file="$runtime_dir/init-$$-$RANDOM"
        history_file="$runtime_dir/history-$$-$RANDOM"
        parent_history_file="$runtime_dir/parent-history-$$-$RANDOM"

        umask 077

        history -w "$parent_history_file" || {
            printf 'CVB: unable to create history snapshot: %s\n' "$parent_history_file" >&2
            rm -f "$init_file" "$parent_history_file"
            return 1
        }

        (
            set +o history

            printf '%s\n' '# CVB parent shell state'
            printf '%s\n' '# Generated automatically; do not edit.'

            printf '\n%s\n' '# Aliases'
            alias

            printf '\n%s\n' '# Functions'
            declare -f

            printf '\n%s\n' '# Shell options'
            set +o

            printf '\n%s\n' '# shopt options'
            shopt -p
        ) >"$init_file" || {
            rm -f "$init_file" "$parent_history_file"
            printf 'CVB: unable to create shell state file: %s\n' "$init_file" >&2
            return 1
        }

        : >"$history_file" || {
            printf 'CVB: unable to create history file: %s\n' "$history_file" >&2
            rm -f "$init_file" "$parent_history_file"
            return 1
        }

        export CVB_ACTIVE=1
        export CVB_HISTORY_FILE="$history_file"
        export CVB_PARENT_HISTORY_FILE="$parent_history_file"
        export CVB_MARKER_KEY=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')

        CVB_INIT_FILE="$init_file" command cvb "$@"
        local status=$?

        if [[ -s $history_file ]]; then
            # Flush the parent's pending history first. Entries loaded with
            # `history -r` are not counted among Bash's pending entries.
            history -a

            # Bash writes the final `exit` command directly to HISTFILE when
            # the child terminates, so it never passes through PROMPT_COMMAND.
            # It is CVB control flow, not session history.
            if [[ $(tail -n 1 "$history_file") == exit ]]; then
                sed '$d' "$history_file" >"$history_file.tmp" &&
                    mv "$history_file.tmp" "$history_file"
            fi

            history -r "$history_file"

            # Bash does not write `history -r` entries to HISTFILE on exit.
            # Persist the imported session entries explicitly.
            if [[ -n ${HISTFILE-} ]]; then
                cat "$history_file" >>"$HISTFILE"
            fi
        fi
        rm -f "$history_file"

        rm -f "$parent_history_file"

        rm -f "$init_file"

        if [[ -n $previous_cvb_active ]]; then
            export CVB_ACTIVE="$previous_cvb_active_value"
        else
            unset CVB_ACTIVE
        fi

        if [[ -n $previous_cvb_history_file ]]; then
            export CVB_HISTORY_FILE="$previous_cvb_history_file"
        else
            unset CVB_HISTORY_FILE
        fi

        if [[ -n $previous_cvb_marker_key ]]; then
            export CVB_MARKER_KEY="$previous_cvb_marker_key_value"
        else
            unset CVB_MARKER_KEY
        fi

        return "$status"
    fi

    command cvb "$@"
}
