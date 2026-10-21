use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmResult {
    Confirmed,
    Declined,
}

/// Prompts through caller-supplied streams so confirmation can be tested
/// deterministically without an actual terminal.
pub(crate) fn prompt_confirmation_with<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
) -> Result<ConfirmResult, String> {
    write!(writer, "Apply these changes? [y/N]: ")
        .map_err(|e| format!("CVB: failed to write confirmation prompt: {e}"))?;

    writer
        .flush()
        .map_err(|e| format!("CVB: failed to flush confirmation prompt: {e}"))?;

    let mut line = String::new();
    let bytes_read = reader
        .read_line(&mut line)
        .map_err(|e| format!("CVB: failed to read confirmation response: {e}"))?;

    if bytes_read == 0 {
        return Ok(ConfirmResult::Declined);
    }

    let trimmed = line.trim();
    if trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes") {
        Ok(ConfirmResult::Confirmed)
    } else {
        Ok(ConfirmResult::Declined)
    }
}

/// Reads confirmation directly from /dev/tty, never from stdin.
///
/// Dry-run bypasses terminal access entirely. The caller maps the returned
/// status to the surgery command's process exit code.
pub(crate) fn prompt_confirmation(
    dry_run: bool,
) -> Result<ConfirmResult, (String, i32)> {
    if dry_run {
        return Ok(ConfirmResult::Confirmed);
    }

    let _guard = crate::surgery::keys::SurgeryTerminalGuard::enter()?;

    let tty_in = OpenOptions::new()
        .read(true)
        .open("/dev/tty")
        .map_err(|_| {
            (
                "E-NOTTY: interactive terminal confirmation requires /dev/tty"
                    .to_string(),
                2,
            )
        })?;

    let tty_out = OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .map_err(|_| {
            (
                "E-NOTTY: interactive terminal confirmation requires /dev/tty"
                    .to_string(),
                2,
            )
        })?;

    prompt_confirmation_with(BufReader::new(tty_in), tty_out)
        .map_err(|error| (error, 2))
}

#[cfg(test)]
mod tests {
    use super::{prompt_confirmation, prompt_confirmation_with, ConfirmResult};
    use std::io::Cursor;

    fn prompt(input: &str) -> (Result<ConfirmResult, String>, Vec<u8>) {
        let reader = Cursor::new(input.as_bytes());
        let mut output = Vec::new();
        let result = prompt_confirmation_with(reader, &mut output);
        (result, output)
    }

    #[test]
    fn accepts_yes_variants_and_surrounding_whitespace() {
        for input in ["y\n", "Y\n", "yes\n", "YES\n", " y \n"] {
            let (result, output) = prompt(input);
            assert_eq!(
                result.unwrap(),
                ConfirmResult::Confirmed,
                "input: {input:?}"
            );
            assert_eq!(
                output,
                b"Apply these changes? [y/N]: ",
                "prompt for input: {input:?}"
            );
        }
    }

    #[test]
    fn declines_no_blank_and_arbitrary_responses() {
        for input in ["n\n", "N\n", "no\n", "\n", "cancel\n"] {
            let (result, output) = prompt(input);
            assert_eq!(
                result.unwrap(),
                ConfirmResult::Declined,
                "input: {input:?}"
            );
            assert_eq!(output, b"Apply these changes? [y/N]: ");
        }
    }

    #[test]
    fn treats_eof_as_decline() {
        let (result, output) = prompt("");
        assert_eq!(result.unwrap(), ConfirmResult::Declined);
        assert_eq!(output, b"Apply these changes? [y/N]: ");
    }

    #[test]
    fn dry_run_bypasses_terminal_access() {
        assert_eq!(
            prompt_confirmation(true).unwrap(),
            ConfirmResult::Confirmed
        );
    }
}
