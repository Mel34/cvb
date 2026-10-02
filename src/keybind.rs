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
        return Ok(vec![parse(DEFAULT_KEYBIND)?]);
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

fn parse(value: &str) -> Result<KeyBinding, String> {
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
