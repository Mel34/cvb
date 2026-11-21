//! Which resident session answers the screen-capture hotkey.
//!
//! Every session reads the keyboard through libinput, so one key press reaches
//! all sessions at once. Each would copy its own screen and the last clipboard
//! write would win, whatever window the user meant. This module makes exactly
//! one session answer: the one whose terminal window had the focus most
//! recently.
//!
//! * Each session asks its terminal for focus reports (DECSET 1004). The
//!   terminal then sends `ESC [ I` when its window gains focus and `ESC [ O`
//!   when it loses it. The proxy strips these reports from the input before the
//!   shell or the patch logic sees them.
//! * Each session publishes its focus state in a small file of its own in
//!   `$XDG_RUNTIME_DIR/cvb/focus/`, using the system-wide monotonic clock, so
//!   sessions can be compared without talking to each other.
//! * On a hotkey press every session reads the files of the live sessions and
//!   applies [`answers_hotkey`]: the focused session answers; when no terminal
//!   has the focus (the user is in the browser), the session that was focused
//!   last answers. Nobody else does.
//!
//! A session that cannot read or write the registry answers anyway, so a
//! problem here never loses a capture. `CVB_FOCUS=off` turns the selection off
//! and brings back the old behaviour (every session answers).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Asks the terminal to report focus changes.
pub const ENABLE: &[u8] = b"\x1b[?1004h";
/// Stops focus reports.
pub const DISABLE: &[u8] = b"\x1b[?1004l";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusEvent {
    In,
    Out,
}

/// Removes complete focus reports (`ESC [ I`, `ESC [ O`) from terminal input.
/// Returns the remaining bytes and the reports in order.
pub fn strip_focus_reports(input: &[u8]) -> (Vec<u8>, Vec<FocusEvent>) {
    let mut kept = Vec::with_capacity(input.len());
    let mut events = Vec::new();
    let mut index = 0;

    while index < input.len() {
        if input[index] == 0x1b && index + 2 < input.len() && input[index + 1] == b'[' {
            match input[index + 2] {
                b'I' => {
                    events.push(FocusEvent::In);
                    index += 3;
                    continue;
                }
                b'O' => {
                    events.push(FocusEvent::Out);
                    index += 3;
                    continue;
                }
                _ => {}
            }
        }
        kept.push(input[index]);
        index += 1;
    }

    (kept, events)
}

/// Notices when output from the shell or a program changes the focus-report
/// mode, so the proxy can switch it back on. A full-screen program turns it off
/// when it exits (`ESC [ ? 1004 l`), which would silence this session for good.
#[derive(Default)]
pub struct ModeWatcher {
    tail: Vec<u8>,
}

impl ModeWatcher {
    /// True when `chunk` (together with the end of the previous chunk) holds
    /// the digits `1004`. Plain text with that number also matches; the cost is
    /// one redundant 8-byte write.
    pub fn mentions_mode(&mut self, chunk: &[u8]) -> bool {
        let mut window = std::mem::take(&mut self.tail);
        window.extend_from_slice(chunk);
        let found = window.windows(4).any(|part| part == b"1004");
        let keep = window.len().saturating_sub(3);
        self.tail = window.split_off(keep);
        found
    }
}

/// One session's published focus state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub pid: u32,
    /// The terminal window has the focus right now.
    pub focused: bool,
    /// Monotonic time of the latest focus-in, in nanoseconds.
    pub last_in: u64,
}

/// Decides whether the session `me` answers a hotkey press, given the other
/// live sessions. Pure, so it can be tested without terminals.
pub fn answers_hotkey(me: &Entry, others: &[Entry]) -> bool {
    let everyone = others.iter().chain(std::iter::once(me));
    let key = |entry: &&Entry| (entry.last_in, entry.pid);

    let winner = everyone
        .clone()
        .filter(|entry| entry.focused)
        .max_by_key(key)
        .or_else(|| everyone.max_by_key(key));

    winner.is_some_and(|winner| winner.pid == me.pid)
}

fn monotonic_ns() -> u64 {
    let mut spec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `spec` is a valid, writable timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut spec) };
    (spec.tv_sec as u64) * 1_000_000_000 + spec.tv_nsec as u64
}

/// Start time of a process in clock ticks since boot (`/proc/PID/stat`, field
/// 22). Tells a live session from a recycled process id.
fn process_start_time(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is in parentheses and may hold spaces or parentheses;
    // everything after the last `)` is plain fields, starting at field 3.
    let after_name = stat.rsplit_once(')')?.1;
    after_name.split_whitespace().nth(19)?.parse().ok()
}

fn parse_entry(text: &str) -> Option<(Entry, u64)> {
    let mut fields = text.split_whitespace();
    let pid = fields.next()?.parse().ok()?;
    let focused = fields.next()? == "1";
    let last_in = fields.next()?.parse().ok()?;
    let start_time = fields.next()?.parse().ok()?;
    Some((
        Entry {
            pid,
            focused,
            last_in,
        },
        start_time,
    ))
}

/// This session's entry in the registry directory.
pub struct FocusRegistry {
    dir: PathBuf,
    me: Entry,
    start_time: u64,
}

impl FocusRegistry {
    /// `None` when the selection is off (`CVB_FOCUS=off`) or there is no
    /// runtime directory; the session then answers every hotkey press.
    pub fn new() -> Option<Self> {
        if let Some(value) = std::env::var_os("CVB_FOCUS") {
            let value = value.to_string_lossy().to_ascii_lowercase();
            if matches!(value.as_str(), "off" | "0" | "false" | "no") {
                return None;
            }
        }

        let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty())?;
        let dir = Path::new(&runtime).join("cvb").join("focus");
        Self::with_dir(dir, std::process::id())
    }

    /// Opens the registry in `dir` for the session `pid`. A new session starts
    /// out focused: it was started from the terminal the user is looking at.
    pub fn with_dir(dir: PathBuf, pid: u32) -> Option<Self> {
        fs::create_dir_all(&dir).ok()?;
        let start_time = process_start_time(pid)?;
        let registry = Self {
            dir,
            me: Entry {
                pid,
                focused: true,
                last_in: monotonic_ns(),
            },
            start_time,
        };
        registry.remove_stale();
        registry.publish().ok()?;
        Some(registry)
    }

    pub fn handle(&mut self, event: FocusEvent) {
        match event {
            FocusEvent::In => {
                self.me.focused = true;
                self.me.last_in = monotonic_ns();
            }
            FocusEvent::Out => self.me.focused = false,
        }
        // A registry that cannot be written leaves the old state in place;
        // other sessions then see a stale entry, which is the safe direction.
        let _ = self.publish();
    }

    /// Does this session answer the hotkey that was just pressed? Errors count
    /// as yes.
    pub fn answers_hotkey(&self) -> bool {
        let Some(others) = self.read_others() else {
            return true;
        };
        answers_hotkey(&self.me, &others)
    }

    fn file(&self, pid: u32) -> PathBuf {
        self.dir.join(pid.to_string())
    }

    fn publish(&self) -> std::io::Result<()> {
        let temporary = self.dir.join(format!("{}.tmp", self.me.pid));
        let text = format!(
            "{} {} {} {}\n",
            self.me.pid,
            u8::from(self.me.focused),
            self.me.last_in,
            self.start_time
        );
        let mut file = fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        drop(file);
        fs::rename(temporary, self.file(self.me.pid))
    }

    /// Entries of the other live sessions. Files of sessions that are gone,
    /// including ones whose process id was reused, are deleted.
    fn read_others(&self) -> Option<Vec<Entry>> {
        let mut others = Vec::new();

        for item in fs::read_dir(&self.dir).ok()? {
            let path = item.ok()?.path();
            let Some(pid) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            if pid == self.me.pid {
                continue;
            }

            let live = fs::read_to_string(&path)
                .ok()
                .and_then(|text| parse_entry(&text))
                .filter(|(entry, start_time)| {
                    entry.pid == pid && process_start_time(pid) == Some(*start_time)
                });
            match live {
                Some((entry, _)) => others.push(entry),
                None => {
                    let _ = fs::remove_file(&path);
                }
            }
        }

        Some(others)
    }

    fn remove_stale(&self) {
        let _ = self.read_others();
    }
}

impl Drop for FocusRegistry {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.file(self.me.pid));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, focused: bool, last_in: u64) -> Entry {
        Entry {
            pid,
            focused,
            last_in,
        }
    }

    #[test]
    fn strips_focus_reports_and_keeps_everything_else() {
        let (kept, events) = strip_focus_reports(b"ab\x1b[Ocd\x1b[Ief");
        assert_eq!(kept, b"abcdef");
        assert_eq!(events, vec![FocusEvent::Out, FocusEvent::In]);

        // Other escape sequences and a lone Escape pass through.
        let (kept, events) = strip_focus_reports(b"\x1b[A\x1b\x1b[1;5C\x1b");
        assert_eq!(kept, b"\x1b[A\x1b\x1b[1;5C\x1b");
        assert!(events.is_empty());

        let (kept, events) = strip_focus_reports(b"\x1b[I");
        assert!(kept.is_empty());
        assert_eq!(events, vec![FocusEvent::In]);

        // A report cut in two reads is not recognized and passes through.
        let (kept, events) = strip_focus_reports(b"\x1b[");
        assert_eq!(kept, b"\x1b[");
        assert!(events.is_empty());
    }

    #[test]
    fn notices_the_mode_number_even_across_reads() {
        let mut watcher = ModeWatcher::default();
        assert!(!watcher.mentions_mode(b"hello \x1b[?25h"));
        assert!(watcher.mentions_mode(b"\x1b[?1004l"));
        assert!(!watcher.mentions_mode(b"plain"));
        assert!(!watcher.mentions_mode(b"\x1b[?10"));
        assert!(watcher.mentions_mode(b"04l"));
        // The same digits are not found twice.
        assert!(!watcher.mentions_mode(b"x"));
    }

    #[test]
    fn the_focused_session_answers_and_nobody_else() {
        let me = entry(10, true, 500);
        let other = entry(20, false, 900);
        assert!(answers_hotkey(&me, std::slice::from_ref(&other)));
        assert!(!answers_hotkey(&other, &[me]));
    }

    #[test]
    fn without_focus_the_most_recently_focused_session_answers() {
        let old = entry(10, false, 100);
        let newer = entry(20, false, 200);
        assert!(!answers_hotkey(&old, std::slice::from_ref(&newer)));
        assert!(answers_hotkey(&newer, &[old]));
    }

    #[test]
    fn a_single_session_always_answers() {
        assert!(answers_hotkey(&entry(10, false, 0), &[]));
        assert!(answers_hotkey(&entry(10, true, 5), &[]));
    }

    #[test]
    fn two_focused_entries_resolve_to_the_later_focus_in() {
        let a = entry(10, true, 100);
        let b = entry(20, true, 200);
        assert!(!answers_hotkey(&a, std::slice::from_ref(&b)));
        assert!(answers_hotkey(&b, &[a]));
    }

    #[test]
    fn exactly_one_of_three_sessions_answers() {
        let all = [entry(1, false, 30), entry(2, false, 10), entry(3, true, 20)];
        let answering: Vec<u32> = all
            .iter()
            .filter(|me| {
                let others: Vec<Entry> =
                    all.iter().filter(|e| e.pid != me.pid).cloned().collect();
                answers_hotkey(me, &others)
            })
            .map(|me| me.pid)
            .collect();
        assert_eq!(answering, vec![3]);
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cvb-focus-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn registry_follows_focus_events_and_ignores_dead_sessions() {
        let dir = temp_dir("registry");
        let me = std::process::id();
        let parent = std::os::unix::process::parent_id();

        let mut registry = FocusRegistry::with_dir(dir.clone(), me).unwrap();
        assert!(registry.answers_hotkey(), "alone, and focused at start");

        // The parent process stands in for a second live session.
        let parent_start = process_start_time(parent).unwrap();
        let publish_parent = |focused: u8, last_in: u64| {
            fs::write(
                dir.join(parent.to_string()),
                format!("{parent} {focused} {last_in} {parent_start}\n"),
            )
            .unwrap();
        };

        publish_parent(1, monotonic_ns() + 1_000_000_000);
        registry.handle(FocusEvent::Out);
        assert!(!registry.answers_hotkey(), "the other session has the focus");

        publish_parent(0, 1);
        assert!(registry.answers_hotkey(), "nobody focused; this one was last");

        registry.handle(FocusEvent::In);
        publish_parent(0, 1);
        assert!(registry.answers_hotkey());

        // A file of a finished process is deleted and ignored.
        let mut finished = std::process::Command::new("true").spawn().unwrap();
        let dead = finished.id();
        finished.wait().unwrap();
        fs::write(dir.join(dead.to_string()), format!("{dead} 1 {} 1\n", u64::MAX)).unwrap();
        assert!(registry.answers_hotkey());
        assert!(!dir.join(dead.to_string()).exists());

        // A recycled process id (wrong start time) is ignored too.
        fs::write(
            dir.join(parent.to_string()),
            format!("{parent} 1 {} {}\n", u64::MAX, parent_start + 1),
        )
        .unwrap();
        assert!(registry.answers_hotkey());
        assert!(!dir.join(parent.to_string()).exists());

        drop(registry);
        assert!(!dir.join(me.to_string()).exists(), "own file removed on exit");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_registry_lets_the_session_answer() {
        let dir = temp_dir("unreadable");
        let registry = FocusRegistry::with_dir(dir.clone(), std::process::id()).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert!(registry.answers_hotkey());
    }
}
