#!/usr/bin/env bash
# Tests the Bash tab completion for cvb (data/cvb.bash) without a terminal: the
# completion function is called with the variables Bash would set.
set -u

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

repo=$tmp/repo
mkdir -p "$repo/src/units" "$repo/docs" "$repo/target" "$tmp/plain"
(
    cd "$repo" || exit 1
    git init -q
    printf 'target/\n' >.gitignore
    : >README.md
    : >src/main.rs
    : >src/pull.rs
    : >src/units/a.rs
    : >docs/guide.md
    : >target/ignored.rs
    git add -A
    git -c user.name=t -c user.email=t@t commit -q -m init
    : >new.rs # untracked and not ignored
) || exit 1

# shellcheck source=../data/cvb.bash
source "$root/data/cvb.bash"

COMPOPT=
compopt() { COMPOPT="$*"; }

passed=0
failed=0

# complete 'LINE' -> sorted candidates, one per line; sets COMPOPT.
complete_line() {
    COMP_LINE=$1
    COMP_POINT=${#1}
    COMPOPT=
    _cvb_complete
    ((${#COMPREPLY[@]})) && printf '%s\n' "${COMPREPLY[@]}" | sort
}

check() {
    local label=$1 line=$2 expected=$3 actual
    actual=$(complete_line "$line")
    if [[ $actual == "$expected" ]]; then
        passed=$((passed + 1))
    else
        failed=$((failed + 1))
        printf 'FAIL: %s\n  line:     %q\n  expected: %q\n  actual:   %q\n' \
            "$label" "$line" "$expected" "$actual"
    fi
}

check_nospace() {
    local label=$1 line=$2 expected=$3
    complete_line "$line" >/dev/null
    if [[ $COMPOPT == "$expected" ]]; then
        passed=$((passed + 1))
    else
        failed=$((failed + 1))
        printf 'FAIL: %s\n  expected compopt %q, got %q\n' "$label" "$expected" "$COMPOPT"
    fi
}

lines() { printf '%s\n' "$@"; }

cd "$repo" || exit 1
COMP_WORDBREAKS=$' \t\n"\'><=;|&(:'

check 'first word offers subcommands, options and files' 'cvb ' \
    "$(lines --help --version -h -V context pull repaste README.md docs new.rs src target | sort)"
check 'subcommand prefix' 'cvb pu' "$(lines pull)"
check 'option prefix' 'cvb --v' "$(lines --version)"
check 'path argument' 'cvb sr' "$(lines src)"
check 'later path arguments' 'cvb README.md sr' "$(lines src)"
check 'context takes no arguments' 'cvb context ' ''
check 'repaste takes no arguments' 'cvb repaste ' ''
check 'repaste is completed' 'cvb rep' "$(lines repaste)"
check 'pull without arguments' 'cvb pull ' "$(lines callers: cascade: file: head: lines: tail:)"
check 'directive prefix' 'cvb pull fi' "$(lines file:)"
check_nospace 'a lone directive keeps the cursor attached' 'cvb pull fi' '-o nospace'

check 'files after file:' 'cvb pull file:' "$(lines .gitignore README.md docs/ new.rs src/ | sort)"
check 'files in a directory' 'cvb pull file:src/' "$(lines src/main.rs src/pull.rs src/units/)"
check 'file prefix' 'cvb pull file:src/m' "$(lines src/main.rs)"
check 'nested directory' 'cvb pull file:src/units/' "$(lines src/units/a.rs)"
check 'untracked files are offered, ignored ones are not' 'cvb pull file:n' "$(lines new.rs)"
check 'ignored directory' 'cvb pull file:tar' ''
check 'leading ./' 'cvb pull file:./sr' "$(lines ./src/)"
check_nospace 'a directory keeps the cursor attached' 'cvb pull file:sr' '-o nospace'
check 'second file scope' 'cvb pull file:src/main.rs file:src/p' "$(lines src/pull.rs)"

COMP_WORDBREAKS=$' \t\n"\'><=;|&('
check 'whole words when : is not a word break' 'cvb pull file:src/m' "$(lines file:src/main.rs)"
COMP_WORDBREAKS=$' \t\n"\'><=;|&(:'

check 'one file leaves every directive' 'cvb pull file:src/main.rs ' \
    "$(lines callers: cascade: file: head: lines: tail:)"
check 'two files need a term' 'cvb pull file:a file:b ' "$(lines callers: cascade: file:)"
check 'lines: needs exactly one file' 'cvb pull file:a lines:1-5 ' "$(lines head: lines: tail:)"
check 'lines: before the file' 'cvb pull lines:1-5 ' "$(lines file: head: lines: tail:)"
check 'head: excludes tail: and terms' 'cvb pull file:a head:5 ' "$(lines lines:)"
check 'a search term leaves file:, lines: and cascade:' 'cvb pull foo ' "$(lines cascade: file: lines:)"
check 'a +term counts as a term' 'cvb pull +foo -bar ' "$(lines cascade: file: lines:)"
check 'callers: only takes file:' 'cvb pull callers:run ' "$(lines file:)"
check 'callers: with a file' 'cvb pull callers:run file:src/main.rs ' "$(lines file:)"
check 'cascade: depths' 'cvb pull cascade:' "$(lines 1 2 3 4 5)"
check 'cascade: prefix' 'cvb pull cascade:3' "$(lines 3)"
check 'after cascade: only file:' 'cvb pull cascade:2 ' "$(lines file:)"
check 'lines: has no candidates' 'cvb pull file:a lines:' ''
check 'head: has no candidates' 'cvb pull file:a head:' ''
check 'callers: has no candidates' 'cvb pull callers:' ''
check 'a term being typed' 'cvb pull foo' ''

cd "$tmp/plain" || exit 1
check 'outside a Git repository' 'cvb pull file:' ''
cd "$repo" || exit 1

registered=$(complete -p cvb 2>/dev/null)
if [[ $registered == *'-F _cvb_complete cvb' ]]; then
    passed=$((passed + 1))
else
    failed=$((failed + 1))
    printf 'FAIL: cvb.bash registers the completion (got %q)\n' "$registered"
fi

# The session shell gets only the registration; the functions come from the
# parent shell's state.
session=$(bash --norc -c '
    source "$1/data/cvb.bash" >/dev/null 2>&1
    complete -r cvb
    source "$1/data/cvb-control.bash" >/dev/null 2>&1
    complete -p cvb' _ "$root" 2>/dev/null)
if [[ $session == *'-F _cvb_complete cvb' ]]; then
    passed=$((passed + 1))
else
    failed=$((failed + 1))
    printf 'FAIL: the control script registers the completion (got %q)\n' "$session"
fi

printf '%d passed, %d failed\n' "$passed" "$failed"
((failed == 0))
