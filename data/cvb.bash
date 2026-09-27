cvb ()
{
    if [[ "$1" == "on" ]]; then
        if [[ -n ${CVB_ACTIVE:-} ]]; then
            printf 'CVB: already running\n' 1>&2
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

        umask 077

        history -w "$parent_history_file" || {
            printf 'CVB: unable to create history snapshot: %s\n' "$parent_history_file" 1>&2
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
        ) > "$init_file" || {
            rm -f "$init_file" "$parent_history_file"
            printf 'CVB: unable to create shell state file: %s\n' "$init_file" 1>&2
            return 1
        }

        export CVB_ACTIVE=1
        export CVB_HISTORY_FILE="$history_file"
        export CVB_PARENT_HISTORY_FILE="$parent_history_file"

        CVB_INIT_FILE="$init_file" command cvb "$@"
        local status=$?

        if [[ -f $history_file ]]; then
            while IFS= read -r command; do
                command="${command#"${command%%[![:space:]]*}"}"
                history -s "$command"
            done < "$history_file"
            rm -f "$history_file"
        fi

        rm -f "$parent_history_file"

        if [[ $status -ne 0 ]]; then
            rm -f "$init_file"
        fi

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

    if [[ "$1" == "off" ]]; then
        if [[ -n ${CVB_ACTIVE:-} ]]; then
            cvb_control_exit
            exit
        fi

        printf 'CVB: not running\n' 1>&2
        return 1
    fi

    command cvb "$@"
}