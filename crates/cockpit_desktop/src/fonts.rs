//! JetBrains Mono, bundled for code and paths: the page gets it as `data:` URLs in its stylesheet,
//! so the window reads no font file and no network. Its license is `assets/fonts/OFL.txt`.

use base64::engine::general_purpose::STANDARD as BASE64;

/// The family the stylesheet's `--mono` asks for first.
const MONO_FAMILY: &str = "JetBrains Mono";

struct Face {
    weight: u16,
    woff2: &'static [u8],
}

/// Regular and bold: the code's weights. The page picks bold for a weight of 600.
const FACES: [Face; 2] = [
    Face {
        weight: 400,
        woff2: include_bytes!("../assets/fonts/JetBrainsMono-Regular.woff2"),
    },
    Face {
        weight: 700,
        woff2: include_bytes!("../assets/fonts/JetBrainsMono-Bold.woff2"),
    },
];

/// The `@font-face` rules of the bundled faces, to go before the stylesheet.
pub fn font_faces() -> String {
    use std::fmt::Write;

    use base64::Engine;

    let mut css = String::new();
    for face in &FACES {
        let data = BASE64.encode(face.woff2);
        // Writing to a `String` cannot fail.
        let _ = writeln!(
            css,
            r#"@font-face {{ font-family: "{MONO_FAMILY}"; font-style: normal; font-weight: {}; font-display: block; src: url(data:font/woff2;base64,{data}) format("woff2"); }}"#,
            face.weight
        );
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stylesheet_asks_first_for_the_bundled_family() {
        const STYLESHEET: &str = include_str!("style.css");

        let mono = STYLESHEET
            .lines()
            .find_map(|line| line.trim().strip_prefix("--mono:"))
            .map(str::trim);

        assert!(
            mono.is_some_and(|stack| stack.starts_with(&format!("\"{MONO_FAMILY}\""))),
            "`--mono` is {mono:?}"
        );
    }

    #[test]
    fn each_face_is_a_woff2_file() {
        const WOFF2_SIGNATURE: &[u8] = b"wOF2";

        for face in &FACES {
            assert!(face.woff2.starts_with(WOFF2_SIGNATURE));
        }
        assert_eq!(font_faces().lines().count(), FACES.len());
    }
}
