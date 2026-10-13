#!/usr/bin/env bash
set -u

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

run_step() {
    local label=$1
    local logfile=$2
    shift 2

    if "$@" >"$logfile" 2>&1; then
        return 0
    else
        local status=$?
        printf '\nFAIL: %s (exit %s)\n' "$label" "$status" >&2
        cat "$logfile" >&2
        exit "$status"
    fi
}

run_step "cargo test" "$tmpdir/test.log" cargo test

summary=$(awk '
    /test result:/ {
        passed += $4
        failed += $6
        ignored += $8
        found = 1
    }
    END {
        if (!found) exit 1
        printf "%d passed, %d failed, %d ignored", passed, failed, ignored
    }
' "$tmpdir/test.log") || {
    printf 'FAIL: Could not parse cargo test summary. Full output follows:\n' >&2
    cat "$tmpdir/test.log" >&2
    exit 1
}

run_step "command lifecycle" "$tmpdir/lifecycle.log" scripts/test-command-lifecycle.sh
lifecycle_summary=$(tail -n 1 "$tmpdir/lifecycle.log")
printf 'Command lifecycle: %s\n' "$lifecycle_summary"
run_step "CLI bridge" "$tmpdir/cli-bridge.log" scripts/test-cli-bridge.py
cli_bridge_summary=$(tail -n 1 "$tmpdir/cli-bridge.log")
printf 'CLI bridge: %s\n' "$cli_bridge_summary"

run_step "PTY capture" "$tmpdir/pty-capture.log" scripts/test-pty-capture.py
pty_capture_summary=$(grep -E '^(SKIP: interactive PTY unavailable|[0-9]+ passed, [0-9]+ failed)' "$tmpdir/pty-capture.log" | tail -n 1 || true)
if [[ -z "$pty_capture_summary" ]]; then
    printf 'FAIL: PTY harness produced no recognizable summary. Full output follows:\n' >&2
    cat "$tmpdir/pty-capture.log" >&2
    exit 1
fi
printf 'PTY capture: %s\n' "$pty_capture_summary"

run_step "cargo check" "$tmpdir/check.log" cargo check
run_step "git diff --check" "$tmpdir/diff.log" git diff --check

printf 'Tests: %s\n' "$summary"
printf 'cargo check: OK\n'
printf 'git diff --check: OK\n'
