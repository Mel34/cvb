//! Everything between "the recognizer found a patch" and "the user sees the
//! result": the shell's directory, the runner call, the text for the screen, and
//! the key sequence that makes the shell redraw its prompt.

use std::ffi::OsStr;
use std::path::PathBuf;

use crate::patch::paste::RejectReason;
use crate::patch::picker::Picker;
use crate::patch::run::{DEFAULT_TIMEOUT, Env, Report, open_picker, preview_patch, run_patch};
use crate::commit::journal::resolve_state_dir;
use crate::patch::text::is_escape_char;

/// A key sequence nobody types. `data/cvb-control.bash` binds it to readline's
/// `redraw-current-line`; the proxy sends it after printing a result so the
/// prompt, and anything half-typed, is drawn again without running a command.
pub(crate) const REDRAW_SEQUENCE: &[u8] = b"\x1b[9999~";

/// Name of the program that applies the patch (looked up in `PATH`).
pub(crate) const PROGRAM: &str = "apply_patch";

/// Name of the program that applies unified diffs (`git apply`, looked up in `PATH`).
pub(crate) const GIT_PROGRAM: &str = "git";

/// `CVB_PATCH_PASTE=off` (or `0`, `false`, `no`) turns paste-to-apply off.
pub(crate) fn enabled_by(value: Option<&OsStr>) -> bool {
    match value.and_then(OsStr::to_str) {
        Some(text) => !matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        ),
        None => true,
    }
}

pub(crate) fn enabled() -> bool {
    enabled_by(std::env::var_os("CVB_PATCH_PASTE").as_deref())
}

/// The text for a paste that looked like a patch but is not applied.
pub(crate) fn rejection_text(reason: RejectReason) -> String {
    let reason = match reason {
        RejectReason::Incomplete => {
            "incomplete paste: there is no '*** End Patch' line. The clipboard may have \
             truncated it (4 MB limit); split the patch into smaller ones"
        }
        RejectReason::NotUtf8 => "the patch is not valid UTF-8",
    };
    Report::Rejected(reason.to_string()).to_text()
}

/// The shell's current directory, which is the root of the patch.
pub(crate) fn shell_directory(shell_pid: i32) -> Result<PathBuf, String> {
    std::fs::read_link(format!("/proc/{shell_pid}/cwd"))
        .map_err(|e| format!("cannot find the shell's current directory: {e}"))
}

/// Runs `work` with the runner's surroundings for the shell `shell_pid`, or says
/// (as report text) why there are none.
fn with_env<T>(shell_pid: i32, work: impl FnOnce(&Env) -> T) -> Result<T, String> {
    let root = match shell_directory(shell_pid) {
        Ok(root) => root,
        Err(reason) => return Err(Report::Rejected(reason).to_text()),
    };
    let state_base = match resolve_state_dir() {
        Ok(base) => base,
        Err(error) => {
            return Err(
                Report::Rejected(format!("cannot find CVB's state directory: {error}")).to_text()
            );
        }
    };
    let env = Env {
        root: &root,
        state_base: &state_base,
        program: OsStr::new(PROGRAM),
        git: OsStr::new(GIT_PROGRAM),
        timeout: DEFAULT_TIMEOUT,
    };
    Ok(work(&env))
}

/// Applies a pasted patch for the shell with process id `shell_pid`; returns the
/// text to show and copy.
pub(crate) fn apply_for_shell(text: &str, shell_pid: i32) -> String {
    apply_offering_picker(text, shell_pid).0
}

/// Like [`apply_for_shell`]. When the patch failed because hunks do not match
/// the files, also returns the picker that can place them.
pub(crate) fn apply_offering_picker(text: &str, shell_pid: i32) -> (String, Option<Picker>) {
    match with_env(shell_pid, |env| {
        let report = run_patch(text, env);
        let picker = match report {
            Report::Failed { .. } => open_picker(text, env).ok(),
            _ => None,
        };
        (report.to_text(), picker)
    }) {
        Ok(result) => result,
        Err(text) => (text, None),
    }
}

/// Dry run of a corrected patch: `Ok` with the text to show when it would apply,
/// `Err` with the report text when it would not.
pub(crate) fn preview_for_shell(text: &str, shell_pid: i32) -> Result<String, String> {
    match with_env(shell_pid, |env| preview_patch(text, env)) {
        Ok(Report::Applied { files, output }) => {
            Ok(format!("CVB patch would apply to {files} file(s):\n{output}"))
        }
        Ok(other) => Err(other.to_text()),
        Err(text) => Err(text),
    }
}

/// What the review gate shows for a pasted patch: `Ok` is the screen text of a
/// patch that parses (nothing is copied), `Err` is the rejection report for one
/// that does not, which is shown and copied like any other result.
pub(crate) fn review_screen(text: &str) -> Result<String, String> {
    match crate::patch::parse::parse_any(text) {
        Ok(parsed) => Ok(crate::patch::review::display(text, &parsed)),
        Err(error) => Err(Report::Rejected(error.to_string()).to_text()),
    }
}

/// Bytes to write to the terminal for a result: a fresh line, the text with
/// control characters made visible, and carriage returns (the terminal is raw).
/// The clipboard gets the plain text; only the screen is escaped.
pub(crate) fn screen_bytes(text: &str) -> Vec<u8> {
    let mut out = String::from("\r\n");
    for ch in text.chars() {
        match ch {
            '\n' => out.push_str("\r\n"),
            '\t' => out.push('\t'),
            c if is_escape_char(c) => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    if !out.ends_with("\r\n") {
        out.push_str("\r\n");
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_feature_is_on_unless_switched_off() {
        assert!(enabled_by(None));
        assert!(enabled_by(Some(OsStr::new("on"))));
        assert!(enabled_by(Some(OsStr::new(""))));
        for off in ["off", "OFF", " 0 ", "false", "No"] {
            assert!(!enabled_by(Some(OsStr::new(off))), "{off:?}");
        }
    }

    #[test]
    fn screen_text_starts_on_a_fresh_line_and_uses_carriage_returns() {
        assert_eq!(screen_bytes("a\nb\n"), b"\r\na\r\nb\r\n");
        assert_eq!(screen_bytes("a"), b"\r\na\r\n");
        assert_eq!(screen_bytes("\tx\n"), b"\r\n\tx\r\n");
    }

    #[test]
    fn screen_text_never_carries_terminal_control_characters() {
        let hostile = "ok\u{1b}]0;title\u{7}\u{1b}[2J\u{9b}x\u{202e}y\u{0}\n";
        let bytes = screen_bytes(hostile);
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains('\u{1b}') && !text.contains('\u{7}') && !text.contains('\u{9b}'));
        assert!(!text.contains('\u{202e}') && !text.contains('\0'));
        assert!(text.contains("\\u{1b}") && text.contains("\\u{7}"), "{text}");
    }

    /// The bytes also feed CVB's screen model (hotkey screenshots), so they must
    /// make a sane screen: a fresh row for each line, starting in column 0.
    #[test]
    fn screen_text_makes_a_sane_screen() {
        let mut screen = vt100::Parser::new(8, 40, 0);
        screen.process(b"\x1b[31m\xe2\x97\x8f\x1b[0m user$ ");
        screen.process(&screen_bytes("CVB patch APPLIED files=1\nSuccess.\nM /p/a\x1b[2J\n"));
        let contents = screen.screen().contents();
        let rows: Vec<&str> = contents.lines().collect();
        assert_eq!(
            rows,
            vec![
                "\u{25cf} user$ ",
                "CVB patch APPLIED files=1",
                "Success.",
                "M /p/a\\u{1b}[2J",
            ]
        );
    }

    #[test]
    fn rejection_texts_follow_the_contract_and_name_the_remedy() {
        let text = rejection_text(RejectReason::Incomplete);
        assert!(text.starts_with("CVB patch REJECTED: incomplete paste"), "{text}");
        assert!(text.contains("4 MB") && text.contains("split"), "{text}");
        assert!(rejection_text(RejectReason::NotUtf8).contains("not valid UTF-8"));
    }

    #[test]
    fn review_text_for_a_good_patch_is_screen_only_and_for_a_bad_one_a_rejection() {
        let good = "*** Begin Patch\n*** Delete File: a\n*** End Patch";
        let shown = review_screen(good).unwrap();
        assert!(shown.contains("CVB patch REVIEW (apply_patch, 3 lines, 1 file): delete a"), "{shown}");
        let bad = "*** Begin Patch\n*** Delete File: ../a\n*** End Patch";
        let report = review_screen(bad).unwrap_err();
        assert!(report.starts_with("CVB patch REJECTED: line 2: unsafe path"), "{report}");
    }

    #[test]
    fn the_shells_directory_is_read_from_proc() {
        let here = std::env::current_dir().unwrap();
        assert_eq!(shell_directory(std::process::id() as i32).unwrap(), here);
        assert!(shell_directory(i32::MAX).is_err());
    }

    #[test]
    fn the_redraw_sequence_is_a_single_private_escape_sequence() {
        assert_eq!(REDRAW_SEQUENCE, b"\x1b[9999~");
    }
}
