use std::fs;
use std::path::PathBuf;

const DEFAULT_KEYBIND: &str = "Ctrl+Shift+B";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBinding {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_key: bool,
    pub key: Key,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Character(char),
    Enter,
    Escape,
    Tab,
    Space,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Left,
    Right,
    Up,
    Down,
    F(u8),
    Plus,
    Minus,
}

pub fn load() -> Result<Vec<KeyBinding>, String> {
    let path = config_path()?;

    if !path.exists() {
        let parent = path
            .parent()
            .ok_or_else(|| "CVB: unable to determine keybind directory".to_string())?;

        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "CVB: unable to create keybind directory {}: {error}",
                parent.display()
            )
        })?;

        fs::write(&path, format!("{DEFAULT_KEYBIND}\n")).map_err(|error| {
            format!(
                "CVB: unable to create keybind file {}: {error}",
                path.display()
            )
        })?;
    }

    let contents = fs::read_to_string(&path).map_err(|error| {
        format!(
            "CVB: unable to read keybind file {}: {error}",
            path.display()
        )
    })?;

    let mut bindings = Vec::new();

    for (line_number, line) in contents.lines().enumerate() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        bindings.push(
            parse(line).map_err(|error| {
                format!(
                    "CVB: invalid keybind at {}:{}: {error}",
                    path.display(),
                    line_number + 1
                )
            })?,
        );
    }

    if bindings.is_empty() {
        return Ok(vec![parse(DEFAULT_KEYBIND)?]);
    }

    Ok(bindings)
}
fn config_path() -> Result<PathBuf, String> {
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .ok_or_else(|| "CVB: unable to determine config directory".to_string())?;

    Ok(config_dir.join("cvb").join("keybind"))
}

pub fn parse(value: &str) -> Result<KeyBinding, String> {
    let parts = value.split('+').collect::<Vec<_>>();

    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return Err("expected modifiers and a key separated by '+'".to_string());
    }

    if parts.len() < 2 {
        return Err("expected at least one modifier and a key".to_string());
    }

    let mut binding = KeyBinding {
        ctrl: false,
        shift: false,
        alt: false,
        super_key: false,
        key: parse_key(parts[parts.len() - 1])?,
    };

    for modifier in &parts[..parts.len() - 1] {
        match *modifier {
            "Ctrl" => binding.ctrl = true,
            "Shift" => binding.shift = true,
            "Alt" => binding.alt = true,
            "Super" => binding.super_key = true,
            _ => return Err(format!("unknown modifier '{modifier}'")),
        }
    }

    Ok(binding)
}

fn parse_key(value: &str) -> Result<Key, String> {
    match value {
        "Enter" => Ok(Key::Enter),
        "Escape" => Ok(Key::Escape),
        "Tab" => Ok(Key::Tab),
        "Space" => Ok(Key::Space),
        "Backspace" => Ok(Key::Backspace),
        "Delete" => Ok(Key::Delete),
        "Insert" => Ok(Key::Insert),
        "Home" => Ok(Key::Home),
        "End" => Ok(Key::End),
        "PageUp" => Ok(Key::PageUp),
        "PageDown" => Ok(Key::PageDown),
        "Left" => Ok(Key::Left),
        "Right" => Ok(Key::Right),
        "Up" => Ok(Key::Up),
        "Down" => Ok(Key::Down),
        "Plus" => Ok(Key::Plus),
        "Minus" => Ok(Key::Minus),
        "Comma" => Ok(Key::Character(',')),
        "Period" => Ok(Key::Character('.')),
        "Slash" => Ok(Key::Character('/')),
        "Backslash" => Ok(Key::Character('\\')),
        "Semicolon" => Ok(Key::Character(';')),
        "Apostrophe" => Ok(Key::Character('\'')),
        "LeftBracket" => Ok(Key::Character('[')),
        "RightBracket" => Ok(Key::Character(']')),
        "Backtick" => Ok(Key::Character('`')),
        value if value.starts_with('F') => {
            let number = value[1..]
                .parse::<u8>()
                .map_err(|_| format!("invalid function key '{value}'"))?;

            if !(1..=24).contains(&number) {
                return Err(format!("unsupported function key '{value}'"));
            }

            Ok(Key::F(number))
        }
        value if value.chars().count() == 1 => Ok(Key::Character(value.chars().next().unwrap())),
        _ => Err(format!("unknown key '{value}'")),
    }
}

pub struct KeybindWatcher {
    fd: std::os::fd::RawFd,
    watch_descriptor: libc::c_int,
}

impl KeybindWatcher {
    pub fn new() -> Result<Self, String> {
        let path = config_path()?;

        let parent = path
            .parent()
            .ok_or_else(|| "CVB: unable to determine keybind directory".to_string())?;

        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "CVB: unable to create keybind directory {}: {error}",
                parent.display()
            )
        })?;

        let fd = unsafe {
            libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC)
        };

        if fd < 0 {
            return Err(format!(
                "CVB: unable to create keybind file watcher: {}",
                std::io::Error::last_os_error()
            ));
        }

        let parent_c = std::ffi::CString::new(parent.as_os_str().as_encoded_bytes())
            .map_err(|_| "CVB: keybind directory contains an invalid path".to_string())?;

        let watch_descriptor = unsafe {
            libc::inotify_add_watch(
                fd,
                parent_c.as_ptr(),
                libc::IN_CLOSE_WRITE
                    | libc::IN_MOVED_TO
                    | libc::IN_CREATE
                    | libc::IN_DELETE,
            )
        };

        if watch_descriptor < 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(format!(
                "CVB: unable to watch keybind directory {}: {error}",
                parent.display()
            ));
        }

        Ok(Self {
            fd,
            watch_descriptor,
        })
    }

    pub fn fd(&self) -> std::os::fd::RawFd {
        self.fd
    }


    pub fn changed(&self) -> Result<bool, String> {
        let mut buffer = [0u8; 4096];
        let mut changed = false;

        loop {
            let result = unsafe {
                libc::read(
                    self.fd,
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };

            if result < 0 {
                let error = std::io::Error::last_os_error();

                if error.kind() == std::io::ErrorKind::WouldBlock {
                    break;
                }

                return Err(format!(
                    "CVB: unable to read keybind file watcher: {error}"
                ));
            }

            if result == 0 {
                break;
            }

            let mut offset = 0usize;
            let length = result as usize;

            while offset < length {
                let event = unsafe {
                    &*(buffer.as_ptr().add(offset) as *const libc::inotify_event)
                };

                if event.len > 0 {
                    let name_offset =
                        offset + std::mem::size_of::<libc::inotify_event>();
                    let name_length = event.len as usize;

                    if name_offset + name_length <= length {
                        let name = &buffer[name_offset..name_offset + name_length];
                        let name_length = name
                            .iter()
                            .position(|byte| *byte == 0)
                            .unwrap_or(name.len());

                        if name[..name_length] == b"keybind"[..] {
                            changed = true;
                        }
                    }
                }

                let event_size = std::mem::size_of::<libc::inotify_event>()
                    + event.len as usize;

                if event_size == 0 {
                    break;
                }

                offset += event_size;
            }
        }

        Ok(changed)
    }
}

impl Drop for KeybindWatcher {
    fn drop(&mut self) {
        if self.watch_descriptor >= 0 {
            unsafe {
                libc::inotify_rm_watch(self.fd, self.watch_descriptor);
            }
        }

        if self.fd >= 0 {
            unsafe {
                libc::close(self.fd);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ctrl_shift_b() {
        assert_eq!(
            parse("Ctrl+Shift+B").unwrap(),
            KeyBinding {
                ctrl: true,
                shift: true,
                alt: false,
                super_key: false,
                key: Key::Character('B'),
            }
        );
    }

    #[test]
    fn parses_all_modifiers() {
        assert_eq!(
            parse("Ctrl+Shift+Alt+Super+B").unwrap(),
            KeyBinding {
                ctrl: true,
                shift: true,
                alt: true,
                super_key: true,
                key: Key::Character('B'),
            }
        );
    }

    #[test]
    fn parses_named_keys() {
        assert_eq!(parse("Ctrl+Enter").unwrap().key, Key::Enter);
        assert_eq!(parse("Ctrl+Plus").unwrap().key, Key::Plus);
        assert_eq!(parse("Ctrl+Minus").unwrap().key, Key::Minus);
        assert_eq!(parse("Alt+F12").unwrap().key, Key::F(12));
        assert_eq!(parse("Ctrl+Comma").unwrap().key, Key::Character(','));
        assert_eq!(parse("Ctrl+Period").unwrap().key, Key::Character('.'));
        assert_eq!(parse("Ctrl+Slash").unwrap().key, Key::Character('/'));
        assert_eq!(
            parse("Ctrl+Backslash").unwrap().key,
            Key::Character('\\')
        );
        assert_eq!(
            parse("Ctrl+Semicolon").unwrap().key,
            Key::Character(';')
        );
        assert_eq!(
            parse("Ctrl+Apostrophe").unwrap().key,
            Key::Character('\'')
        );
        assert_eq!(
            parse("Ctrl+LeftBracket").unwrap().key,
            Key::Character('[')
        );
        assert_eq!(
            parse("Ctrl+RightBracket").unwrap().key,
            Key::Character(']')
        );
        assert_eq!(
            parse("Ctrl+Backtick").unwrap().key,
            Key::Character('`')
        );
    }

    #[test]
    fn rejects_missing_key() {
        assert!(parse("Ctrl+Shift").is_err());
    }

    #[test]
    fn rejects_missing_modifier() {
        assert!(parse("B").is_err());
    }

    #[test]
    fn rejects_empty_component() {
        assert!(parse("Ctrl++B").is_err());
    }

    #[test]
    fn rejects_unknown_modifier() {
        assert!(parse("Meta+B").is_err());
    }

    #[test]
    fn rejects_invalid_function_key() {
        assert!(parse("Ctrl+F25").is_err());
    }
}
