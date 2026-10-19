# Fingerprint the integration script loaded into this parent shell.
CVB_BASH_SOURCE=${BASH_SOURCE[0]}
CVB_BASH_SHA256=$(sha256sum -- "$CVB_BASH_SOURCE")
CVB_BASH_SHA256=${CVB_BASH_SHA256%% *}
export CVB_BASH_SOURCE CVB_BASH_SHA256

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

        mkdir -p "$runtime_dir" || return 1

        init_file="$runtime_dir/init-$$-$RANDOM"
        history_file="$runtime_dir/history-$$-$RANDOM"
        parent_history_file="$runtime_dir/parent-history-$$-$RANDOM"

        local previous_umask
        previous_umask=$(umask)

        umask 077

        history -w "$parent_history_file" || {
            printf 'CVB: unable to create history snapshot: %s\n' "$parent_history_file" >&2
            rm -f "$init_file" "$parent_history_file"
            umask "$previous_umask"
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
            umask "$previous_umask"
            printf 'CVB: unable to create shell state file: %s\n' "$init_file" >&2
            return 1
        }

        : >"$history_file" || {
            printf 'CVB: unable to create history file: %s\n' "$history_file" >&2
            rm -f "$init_file" "$parent_history_file"
            umask "$previous_umask"
            return 1
        }

        umask "$previous_umask"

        export CVB_ACTIVE=1
        export CVB_HISTORY_FILE="$history_file"
        export CVB_PARENT_HISTORY_FILE="$parent_history_file"

        CVB_INIT_FILE="$init_file" command cvb "$@"
        local status=$?

        if [[ -s $history_file ]]; then
            # Flush the parent's pending history first. Entries loaded with
            # `history -r` are not counted among Bash's pending entries.
            history -a
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

        return "$status"
    fi

    command cvb "$@"
}
