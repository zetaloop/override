#[path = "unicode_data.rs"]
mod data;

fn contains(ranges: &[(u32, u32)], character: char) -> bool {
    let value = u32::from(character);
    ranges
        .get(ranges.partition_point(|&(_, end)| end < value))
        .is_some_and(|&(start, _)| start <= value)
}

pub mod ident {
    pub const UNICODE_VERSION: (u8, u8, u8) = super::data::VERSION;

    pub fn is_xid_start(character: char) -> bool {
        if character.is_ascii() {
            character.is_ascii_alphabetic()
        } else {
            super::contains(super::data::XID_START, character)
        }
    }

    pub fn is_xid_continue(character: char) -> bool {
        if character.is_ascii() {
            character.is_ascii_alphanumeric() || character == '_'
        } else {
            super::contains(super::data::XID_CONTINUE, character)
        }
    }
}

pub mod properties {
    pub const UNICODE_VERSION: (u64, u64, u64) = (
        super::data::VERSION.0 as u64,
        super::data::VERSION.1 as u64,
        super::data::VERSION.2 as u64,
    );

    pub trait UnicodeEmoji: Copy {
        fn is_emoji_char(self) -> bool;
    }

    impl UnicodeEmoji for char {
        fn is_emoji_char(self) -> bool {
            super::contains(super::data::EMOJI, self)
        }
    }
}
