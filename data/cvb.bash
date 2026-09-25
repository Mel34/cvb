cvb() {
    if [[ "$1" == "on" ]]; then
        local runtime_dir="${XDG_RUNTIME_DIR:-/tmp}/cvb"
        local init_file

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

        CVB_INIT_FILE="$init_file" command cvb "$@"
        local status=$?

        if [[ $status -ne 0 ]]; then
            rm -f "$init_file"
        fi

        return "$status"
    fi

    command cvb "$@"
}