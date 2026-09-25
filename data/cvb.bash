cvb() {
    if [[ "$1" == "on" ]]; then
        if [[ -n ${CVB_ACTIVE:-} ]]; then
            printf 'CVB: already running\n' >&2
            return 1
        fi

        local runtime_dir="${XDG_RUNTIME_DIR:-/tmp}/cvb"
        local init_file
        local previous_cvb_active=${CVB_ACTIVE+x}
        local previous_cvb_active_value=${CVB_ACTIVE-}

        mkdir -p "$runtime_dir" || return 1

        init_file="$runtime_dir/init-$$-$RANDOM"

        umask 077

        {
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
        } >"$init_file" || {
            rm -f "$init_file"
            printf 'CVB: unable to create shell state file: %s\n' "$init_file" >&2
            return 1
        }

        export CVB_ACTIVE=1

        CVB_INIT_FILE="$init_file" command cvb "$@"
        local status=$?

        if [[ $status -ne 0 ]]; then
            rm -f "$init_file"
        fi

        if [[ -n $previous_cvb_active ]]; then
            export CVB_ACTIVE="$previous_cvb_active_value"
        else
            unset CVB_ACTIVE
        fi

        return "$status"
    fi

    if [[ "$1" == "off" && -n ${CVB_ACTIVE:-} ]]; then
        cvb_control_exit
        exit
    fi

    command cvb "$@"
}