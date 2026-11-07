use std::fmt::Write;

use unicode_properties::UnicodeEmoji;

pub fn generate() -> r#override::Result<String> {
    let version = unicode_ident::UNICODE_VERSION;
    let mut output = format!("pub const VERSION: (u8, u8, u8) = {version:?};\n");
    for (name, property) in [
        ("XID_START", unicode_ident::is_xid_start as fn(char) -> bool),
        ("XID_CONTINUE", unicode_ident::is_xid_continue),
        ("EMOJI", char::is_emoji_char),
    ] {
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for character in (0..=u32::from(char::MAX)).filter_map(char::from_u32) {
            if !property(character) {
                continue;
            }
            let value = u32::from(character);
            if let Some((_, end)) = ranges.last_mut()
                && *end + 1 == value
            {
                *end = value;
            } else {
                ranges.push((value, value));
            }
        }
        writeln!(output, "\npub const {name}: &[(u32, u32)] = &[")?;
        for (start, end) in &ranges {
            writeln!(output, "    (0x{start:X}, 0x{end:X}),")?;
        }
        writeln!(output, "];")?;
    }
    Ok(output)
}
