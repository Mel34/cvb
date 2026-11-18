//! Which characters are made visible (as an escape such as `\u{200b}`) when text is shown on a terminal.

pub(crate) fn is_zero_width(c: char) -> bool {
    let code = c as u32;
    matches!(
        code,
        0x200B..=0x200D | 0xFEFF | 0x200E | 0x200F | 0x2028 | 0x2029 | 0x2060..=0x2064
    )
}

pub(crate) fn is_bidi_override(c: char) -> bool {
    let code = c as u32;
    matches!(code, 0x202A..=0x202E | 0x2066..=0x2069)
}

fn is_format_char(c: char) -> bool {
    let code = c as u32;
    matches!(
        code,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

fn is_space_separator(c: char) -> bool {
    matches!(
        c as u32,
        0x00A0 | 0x1680 | 0x2000..=0x200A | 0x202F | 0x205F | 0x3000
    )
}

pub(crate) fn is_control_char(c: char) -> bool {
    let code = c as u32;
    (code < 32 && c != '\t' && c != '\n')
        || code == 127
        || (0x80..=0x9F).contains(&code)
}

pub(crate) fn is_escape_char(c: char) -> bool {
    is_format_char(c) || is_space_separator(c) || is_zero_width(c) || is_control_char(c)
}
