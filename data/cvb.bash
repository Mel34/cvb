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

# Tab completion for cvb.
#
# First word: the subcommands, the options and files and directories (cvb
# captures them). After `cvb pull`: the directives that still make sense for
# what is already typed, and for `file:` the files that pull can search (the
# files Git tracks plus untracked ones it does not ignore), one path segment
# at a time like ordinary file completion. `cascade:` offers its depths.
# Search terms are never completed.
#
# Bash treats ':' as a word break, which would cut `file:src/main.rs` into
# three words, so the words are rebuilt from the command line.
_cvb_complete() {
    COMPREPLY=()

    local line=${COMP_LINE:0:COMP_POINT}
    local -a words=()
    read -ra words <<<"$line"
    [[ $line == *[[:space:]] ]] && words+=("")

    local count=${#words[@]}
    local cur=
    (( count > 0 )) && cur=${words[count-1]}

    if (( count <= 2 )); then
        COMPREPLY=($(compgen -W 'pull context --help --version -h -V' -- "$cur"))
        _cvb_complete_paths "$cur"
        COMPREPLY+=("${COMPREPLY_PATHS[@]}")
        compopt -o filenames 2>/dev/null
        return 0
    fi

    case ${words[1]} in
        pull)
            _cvb_complete_pull "$cur" "${words[@]:2:count-3}"
            _cvb_ltrim_colon "$cur"
            if (( ${#COMPREPLY[@]} == 1 )) && [[ ${COMPREPLY[0]} == *[:/] ]]; then
                compopt -o nospace 2>/dev/null
            fi
            ;;
        context | --help | -h | --version | -V) ;;
        *)
            _cvb_complete_paths "$cur"
            COMPREPLY=("${COMPREPLY_PATHS[@]}")
            compopt -o filenames 2>/dev/null
            ;;
    esac

    return 0
}

# Paths matching $1 into COMPREPLY_PATHS; like Bash's own completion, hidden
# files are offered only when the last path component starts with a dot.
_cvb_complete_paths() {
    local base=${1##*/}
    COMPREPLY_PATHS=()
    local path
    while IFS= read -r path; do
        [[ $base != .* && ${path##*/} == .* ]] && continue
        COMPREPLY_PATHS+=("$path")
    done < <(compgen -f -- "$1")
}

# $1 is the word being completed, the rest are the pull arguments before it.
_cvb_complete_pull() {
    local cur=$1
    shift

    case $cur in
        file:*)
            _cvb_complete_pull_files "${cur#file:}"
            return
            ;;
        cascade:*)
            COMPREPLY=($(compgen -W 'cascade:1 cascade:2 cascade:3 cascade:4 cascade:5' -- "$cur"))
            return
            ;;
        lines:* | head:* | tail:* | callers:*)
            return
            ;;
    esac

    local word
    local files=0 terms=0
    local have_lines=0 have_head=0 have_tail=0 have_callers=0 have_cascade=0

    for word in "$@"; do
        case $word in
            file:*) files=$((files + 1)) ;;
            lines:*) have_lines=1 ;;
            head:*) have_head=1 ;;
            tail:*) have_tail=1 ;;
            callers:*) have_callers=1 ;;
            cascade:*) have_cascade=1 ;;
            *) terms=$((terms + 1)) ;;
        esac
    done

    local trim=$((have_head + have_tail))
    local -a allowed=()

    # file: repeats for a search; lines: and head:/tail: need exactly one.
    if (( !(have_lines && files >= 1) && !(trim && files >= 1) )); then
        allowed+=(file:)
    fi
    if (( !have_callers && !have_cascade && files <= 1 )); then
        allowed+=(lines:)
    fi
    if (( !have_callers && !have_cascade && !trim && terms == 0 && files <= 1 )); then
        allowed+=(head: tail:)
    fi
    if (( !have_callers && !have_lines && !trim && !have_cascade && terms == 0 )); then
        allowed+=(callers:)
    fi
    if (( !have_callers && !have_lines && !trim && !have_cascade )); then
        allowed+=(cascade:)
    fi

    COMPREPLY=($(compgen -W "${allowed[*]}" -- "$cur"))
    if (( ${#COMPREPLY[@]} == 1 )); then
        compopt -o nospace 2>/dev/null
    fi
}

# $1 is the path typed after `file:`. Offers the next path segment of every
# matching file, with a slash after directories, as `file:PATH` words.
_cvb_complete_pull_files() {
    local typed=$1
    local lead=

    if [[ $typed == ./* ]]; then
        lead=./
        typed=${typed#./}
    fi

    mapfile -t COMPREPLY < <(
        git -c core.quotepath=off ls-files --cached --others --exclude-standard 2>/dev/null |
            CVB_TYPED=$typed CVB_LEAD=$lead awk '
                BEGIN { typed = ENVIRON["CVB_TYPED"]; lead = ENVIRON["CVB_LEAD"] }
                index($0, typed) == 1 {
                    rest = substr($0, length(typed) + 1)
                    slash = index(rest, "/")
                    if (slash) rest = substr(rest, 1, slash)
                    word = "file:" lead typed rest
                    if (!(word in seen)) { seen[word] = 1; print word }
                }'
    )
}

# Drops the part of each candidate that Bash already considers typed: with ':'
# in COMP_WORDBREAKS the word being completed starts after the last colon.
_cvb_ltrim_colon() {
    local cur=$1

    if [[ ${COMP_WORDBREAKS-} == *:* && $cur == *:* ]]; then
        local prefix=${cur%"${cur##*:}"}
        local index
        for index in "${!COMPREPLY[@]}"; do
            COMPREPLY[index]=${COMPREPLY[index]#"$prefix"}
        done
    fi
}

complete -F _cvb_complete cvb
