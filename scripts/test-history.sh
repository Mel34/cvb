#!/usr/bin/env bash
# End-to-end tests for the CVB history lifecycle.
#
# Real:    data/cvb.bash (wrapper), data/cvb-control.bash, src/shell.rs (rcfile)
# Stubbed: the Rust PTY proxy. `cvb` becomes "run the generated rcfile in a
# child Bash that reads the same input", which is all the history lifecycle
# depends on.
#
# Needs: bash, rustc. No crates, no terminal, no clipboard.
#
#     scripts/test-history.sh

set -u

root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

pass=0
fail=0

if ! rustc --edition 2024 -o "$work/rcgen" "$root/scripts/rcgen.rs" 2>"$work/rustc.log"; then
    cat "$work/rustc.log"
    echo "FAIL: could not compile scripts/rcgen.rs"
    exit 1
fi

check() {
    local name=$1 expected=$2 actual=$3

    if [[ $expected == "$actual" ]]; then
        pass=$((pass + 1))
        printf 'ok   %s\n' "$name"
    else
        fail=$((fail + 1))
        printf 'FAIL %s\n  expected: %s\n  actual:   %s\n' "$name" "$expected" "$actual"
    fi
}

# run_session NAME HISTAPPEND(on|off) HISTFILE_MODE(set|unset) INPUT
#
# Drives: parent bash -i -> `cvb` wrapper -> child bash -i -> back to parent.
# INPUT is fed on stdin and is consumed first by the parent, then by the child
# (after `cvb`), then by the parent again after the child exits.
run_session() {
    local name=$1 histappend=$2 histfile_mode=$3 input=$4
    local flag=s

    [[ $histappend == off ]] && flag=u

    mkdir -p "$work/$name/home" "$work/$name/run" "$work/$name/bin"
    printf 'old1\nold2\n' >"$work/$name/home/.bash_history"

    # The user's own configuration: applied in both parent and child.
    cat >"$work/$name/home/.bashrc" <<EOF
HISTFILE="$work/$name/home/.bash_history"
HISTCONTROL=ignorespace
shopt -$flag histappend
EOF

    # Stand-in for the real Rust cvb executable: generate the real rcfile,
    # then run the child Bash directly without a PTY proxy.
    # Every invocation appends its argument count to a log, so tests can verify
    # the wrapper starts the session once and never runs `cvb <file>` itself.
    cat >"$work/$name/bin/cvb" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$#" >>"$work/$name/cvb-calls"
exec 3>/dev/null
rc=\$("$work/rcgen") || exit 1
bash --noprofile --rcfile "\$rc" -i
status=\$?
rm -f "\$rc"
exit "\$status"
EOF
    chmod +x "$work/$name/bin/cvb"

    # Optional extra .bashrc content (a test scenario), applied to parent
    # and child.
    if [[ -n ${BASHRC_EXTRA:-} ]]; then
        printf '%s\n' "$BASHRC_EXTRA" >>"$work/$name/home/.bashrc"
    fi

    # Bash loads $HISTFILE itself after the rcfile, so the parent needs no
    # explicit `history -r` here.
    cat >"$work/$name/parent.rc" <<EOF
source "\$HOME/.bashrc"
$([[ $histfile_mode == unset ]] && echo 'unset HISTFILE')
source "$root/data/cvb.bash"
history -c
EOF

    printf '%s\n' "$input" |
        env HOME="$work/$name/home" XDG_RUNTIME_DIR="$work/$name/run" PATH="$work/$name/bin:$PATH" \
            bash --noprofile --rcfile "$work/$name/parent.rc" -i >"$work/$name/out" 2>"$work/$name/err"
}

hist_of() {
    paste -sd'|' "$work/$1/home/.bash_history"
}

leftovers_of() {
    # Session/snapshot/init files must not outlive the session.
    find "$work/$1/run" -type f 2>/dev/null |
        grep -E 'history-|parent-history-|init-|rc-' |
        wc -l |
        tr -d ' '
}

# --------------------------------------------------------------------------
# 1. Normal round trip, histappend on.
#
# The commands typed in the child land in the parent history, in order,
# between `cvb` and the next parent command. A command with a leading space is
# ignored because HISTCONTROL=ignorespace applies in the child as in normal
# Bash history. `exit` itself is not flushed by the child's prompt hook because
# there is no next prompt.
run_session roundtrip-on on set "$(
    cat <<'EOF'
echo before
cvb
echo in1
 echo hidden
echo in2
exit
echo after
exit
EOF
)"
check "round trip (histappend on)" \
    "old1|old2|echo before|cvb|echo in1|echo in2|echo after|exit" \
    "$(hist_of roundtrip-on)"
check "round trip: temp files removed" "0" "$(leftovers_of roundtrip-on)"

# Starting a session invokes cvb exactly once, with no arguments. Context
# initialization belongs to Rust; the wrapper must not run `cvb manual.md`.
check "round trip: wrapper starts cvb once, with no arguments" \
    "0" \
    "$(paste -sd'|' "$work/roundtrip-on/cvb-calls")"

# 2. The same with histappend off. The parent's shopt state is replayed into
# the child, so the lifecycle must not depend on histappend.
run_session roundtrip-off off set "$(
    cat <<'EOF'
echo before
cvb
echo in1
 echo hidden
echo in2
exit
echo after
exit
EOF
)"
check "round trip (histappend off)" \
    "old1|old2|echo before|cvb|echo in1|echo in2|echo after|exit" \
    "$(hist_of roundtrip-off)"

# 3. The child starts with the parent's history loaded, providing normal
# Up-arrow/history parity.
run_session child-has-parent on set "$(
    cat <<'EOF'
cvb
echo CHILDOLD=$(history | grep -c '^ *[0-9]\+ \+old1$')
exit
exit
EOF
)"
check "child sees parent history" \
    "CHILDOLD=1" \
    "$(grep -o 'CHILDOLD=[0-9]*' "$work/child-has-parent/out")"

# 4. A killed child loses only the command that was running: every completed
# command was already flushed at its prompt, and the parent still imports them.
run_session killed on set "$(
    cat <<'EOF'
cvb
echo one
echo two
kill -9 $$
echo after
exit
EOF
)"
check "killed child (SIGKILL)" \
    "old1|old2|cvb|echo one|echo two|echo after|exit" \
    "$(hist_of killed)"

# 5. Multi-line commands round-trip as one entry with Bash's default cmdhist
# behavior. The lifecycle itself does not force cmdhist/lithist settings.
run_session multiline on set "$(
    cat <<'EOF'
cvb
for i in 1; do
echo $i
done
exit
exit
EOF
)"
check "multi-line command" \
    'old1|old2|cvb|for i in 1; do echo $i; done|exit' \
    "$(hist_of multiline)"

# 6. Parent without a HISTFILE: the import still works in memory, and nothing
# is written to disk.
run_session no-histfile on unset "$(
    cat <<'EOF'
cvb
echo in1
exit
echo COUNT=$(history | grep -c 'echo in1$')
exit
EOF
)"
check "parent without HISTFILE: in-memory import" \
    "COUNT=1" \
    "$(grep -o 'COUNT=[0-9]*' "$work/no-histfile/out")"
check "parent without HISTFILE: nothing written" \
    "old1|old2" \
    "$(hist_of no-histfile)"

# 7. A .bashrc that switches history recording on part-way through causes
# Bash to record subsequent sourced rcfile lines. None of CVB's initialization
# may reach the parent's history.
BASHRC_EXTRA=$'set -o history\nalias ll="ls -l"\nmyfunc() {\n    echo hi\n}' \
    run_session bashrc-enables-history on set "$(
        cat <<'EOF'
cvb
echo in1
exit
exit
EOF
    )"
check ".bashrc enables recording: no pollution" \
    "old1|old2|cvb|echo in1|exit" \
    "$(hist_of bashrc-enables-history)"

# --------------------------------------------------------------------------
printf '\n%d passed, %d failed\n' "$pass" "$fail"

if ((fail)); then
    for f in "$work"/*/err; do
        [[ -s $f ]] && {
            printf '\n--- stderr: %s ---\n' "$f"
            cat "$f"
        }
    done
    exit 1
fi
