//! An emoji at the start of an agent's title, which the emoji picker sets. It is part of the
//! title, so it shows wherever the title does, Claude Code's resume list included.

use cockpit_protocol::MAX_TITLE_CHARS;

/// The emoji the picker offers: marks for kinds of work. A variation selector after a symbol
/// asks for its colored form.
pub const TITLE_EMOJI: [&str; 32] = [
    "🚀", "🐛", "🧪", "📦", "🔧", "📝", "🔍", "💡", "⚡", "🔥", "🎯", "🧹", "🛠️", "📊", "🔒", "🌐",
    "🎨", "📚", "🧭", "⏱️", "✅", "❗", "💬", "🤖", "🧩", "🗂️", "🏗️", "🚧", "🧯", "💾", "🧠", "📣",
];

/// What follows the emoji in a title.
const SEPARATOR: char = ' ';

/// Selects the text form (U+FE0E) or the emoji form (U+FE0F) of the symbol before it.
const VARIATION_SELECTORS: [char; 2] = ['\u{fe0e}', '\u{fe0f}'];

/// Joins two emoji into one, as in a family or a profession.
const ZERO_WIDTH_JOINER: char = '\u{200d}';

/// Turns the digit, `#` or `*` before it into a keycap: "1️⃣".
const KEYCAP: char = '\u{20e3}';

/// How many of a title's first characters are read for its emoji. A family of four with skin
/// tones runs to 15.
const MAX_EMOJI_CHARS: usize = 32;

/// The title split into the emoji it starts with and the rest, which starts with the space after
/// the emoji when there is one. `None` when the title starts with no emoji.
pub fn split(title: &str) -> Option<(&str, &str)> {
    let length = emoji_length(title);
    (length > 0).then(|| title.split_at(length))
}

/// The title with `emoji` in place of the one it starts with, or without one for `None`, cut to
/// `MAX_TITLE_CHARS` characters at its end.
pub fn with_emoji(title: &str, emoji: Option<&str>) -> String {
    let rest = split(title).map_or(title, |(_, rest)| rest).trim_start();
    let titled = match emoji {
        Some(emoji) if rest.is_empty() => emoji.to_owned(),
        Some(emoji) => format!("{emoji}{SEPARATOR}{rest}"),
        None => rest.to_owned(),
    };
    titled.chars().take(MAX_TITLE_CHARS).collect()
}

/// The length in bytes of the emoji `text` starts with, 0 when it starts with none. An emoji may
/// span several characters: a variation selector, a skin tone, a keycap, a flag's two letters, or
/// several emoji joined by zero-width joiners.
fn emoji_length(text: &str) -> usize {
    // An emoji is a few characters: the title's first ones are enough.
    let chars: Vec<char> = text.chars().take(MAX_EMOJI_CHARS).collect();
    let Some(&first) = chars.first() else {
        return 0;
    };
    let mut count = 1;
    if is_keycap_base(first) {
        // A digit, `#` or `*` is an emoji only with a keycap after it.
        let selector = chars
            .get(1)
            .is_some_and(|next| VARIATION_SELECTORS.contains(next));
        let keycap_at = if selector { 2 } else { 1 };
        if chars.get(keycap_at) != Some(&KEYCAP) {
            return 0;
        }
        count = keycap_at + 1;
    } else if !is_pictographic(first) {
        return 0;
    } else if is_regional_indicator(first)
        && chars.get(1).copied().is_some_and(is_regional_indicator)
    {
        count = 2;
    }
    loop {
        match chars.get(count) {
            Some(&ZERO_WIDTH_JOINER)
                if chars.get(count + 1).copied().is_some_and(is_pictographic) =>
            {
                count += 2;
            }
            Some(&next) if is_modifier(next) => count += 1,
            _ => break,
        }
    }
    chars
        .iter()
        .take(count)
        .map(|character| character.len_utf8())
        .sum()
}

/// Symbols drawn as emoji: the emoji blocks, and the older symbol blocks they took over.
fn is_pictographic(character: char) -> bool {
    matches!(
        u32::from(character),
        0x231a..=0x23ff
            | 0x2600..=0x27bf
            | 0x2b00..=0x2bff
            | 0x3030
            | 0x303d
            | 0x3297
            | 0x3299
            | 0x1_f000..=0x1_faff
    )
}

/// What may follow an emoji inside the same emoji: a variation selector, a keycap, a skin tone,
/// or a subdivision flag's tag letters.
fn is_modifier(character: char) -> bool {
    VARIATION_SELECTORS.contains(&character)
        || character == KEYCAP
        || matches!(u32::from(character), 0x1_f3fb..=0x1_f3ff | 0xe_0020..=0xe_007f)
}

fn is_regional_indicator(character: char) -> bool {
    matches!(u32::from(character), 0x1_f1e6..=0x1_f1ff)
}

fn is_keycap_base(character: char) -> bool {
    character.is_ascii_digit() || character == '#' || character == '*'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pick_replaces_the_emoji_the_title_starts_with_or_adds_one() {
        assert_eq!(with_emoji("Fix the lexer", Some("🐛")), "🐛 Fix the lexer");
        assert_eq!(
            with_emoji("🐛 Fix the lexer", Some("🚀")),
            "🚀 Fix the lexer"
        );
        assert_eq!(
            with_emoji("🐛Fix the lexer", Some("🚀")),
            "🚀 Fix the lexer"
        );
        assert_eq!(
            with_emoji("🐛   Fix the lexer", Some("🚀")),
            "🚀 Fix the lexer"
        );
        assert_eq!(with_emoji("🐛 Fix the lexer", None), "Fix the lexer");
        assert_eq!(with_emoji("Fix the lexer", None), "Fix the lexer");
        assert_eq!(with_emoji("🐛", Some("🚀")), "🚀");
        assert_eq!(with_emoji("🐛", None), "");
    }

    #[test]
    fn an_emoji_of_several_characters_goes_whole() {
        let titles = [
            "🛠️ Build",
            "🛠 Build",
            "👩🏽‍💻 Build",
            "👨‍👩‍👧 Build",
            "🇫🇷 Build",
            "1️⃣ Build",
            "#⃣ Build",
            "⏱️ Build",
            "✅ Build",
        ];
        for title in titles {
            assert_eq!(with_emoji(title, Some("🚀")), "🚀 Build", "{title}");
        }
    }

    #[test]
    fn text_that_only_looks_like_an_emoji_stays() {
        let titles = [
            "1 Build",
            "#12 Build",
            "→ Build",
            "Build ✅",
            "\u{200d} Build",
        ];
        for title in titles {
            assert_eq!(split(title), None, "{title}");
            assert_eq!(
                with_emoji(title, Some("🚀")),
                format!("🚀 {title}"),
                "{title}"
            );
        }
    }

    #[test]
    fn the_title_keeps_to_the_length_limit_and_keeps_its_emoji() {
        let long = "x".repeat(MAX_TITLE_CHARS);

        let titled = with_emoji(&long, Some("🛠️"));

        assert_eq!(titled.chars().count(), MAX_TITLE_CHARS);
        assert!(titled.starts_with("🛠️ x"));
    }

    #[test]
    fn the_split_keeps_the_space_with_the_rest() {
        assert_eq!(split("🔥 Hot fix"), Some(("🔥", " Hot fix")));
        assert_eq!(split("🗂️ Files"), Some(("🗂️", " Files")));
        assert_eq!(split("Hot fix"), None);
    }

    #[test]
    fn every_offered_emoji_reads_as_one_emoji() {
        for emoji in TITLE_EMOJI {
            assert_eq!(split(emoji), Some((emoji, "")), "{emoji}");
        }
    }
}
