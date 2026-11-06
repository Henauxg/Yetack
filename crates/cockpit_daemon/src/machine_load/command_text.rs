//! A process's command line as the load shows it: the program's file name, then its arguments,
//! on one line, cut at [`MAX_COMMAND_CHARS`].

/// A command line past this many characters ends in an ellipsis.
pub(super) const MAX_COMMAND_CHARS: usize = 120;
const ELLIPSIS: char = '…';

/// `line` cut at [`MAX_COMMAND_CHARS`], with its control characters as spaces.
pub(super) fn shortened(line: &str) -> String {
    let mut short: String = line
        .chars()
        .take(MAX_COMMAND_CHARS)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if line.chars().count() > MAX_COMMAND_CHARS {
        short.push(ELLIPSIS);
    }
    short
}

/// A Windows command line, one string whose program may be in quotes, with the program's path
/// cut to its file name. `None` for an empty line.
#[cfg(any(windows, test))]
pub(super) fn windows_command(line: &str) -> Option<String> {
    let line = line.trim_start();
    let (program, arguments) = match line.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"').unwrap_or((quoted, "")),
        None => line.split_once([' ', '\t']).unwrap_or((line, "")),
    };
    let name = program.rsplit(['\\', '/']).next().unwrap_or(program);
    let arguments = arguments.trim();
    let joined = match (name.is_empty(), arguments.is_empty()) {
        (true, true) => return None,
        (false, true) => name.to_owned(),
        (true, false) => arguments.to_owned(),
        (false, false) => format!("{name} {arguments}"),
    };
    Some(shortened(&joined))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_command_shows_the_programs_file_name_then_its_arguments() {
        assert_eq!(
            windows_command(r#""C:\Program Files\nodejs\node.exe" server.js --port 3000"#),
            Some("node.exe server.js --port 3000".to_owned())
        );
        assert_eq!(
            windows_command(r"C:\Windows\system32\cmd.exe /d /c build.cmd"),
            Some("cmd.exe /d /c build.cmd".to_owned())
        );
        assert_eq!(windows_command("cargo"), Some("cargo".to_owned()));
        assert_eq!(windows_command("  "), None);
    }

    #[test]
    fn a_long_command_is_cut_with_an_ellipsis() {
        let long = format!("rustc {}", "x".repeat(200));

        let shown = windows_command(&long).unwrap();

        assert_eq!(shown.chars().count(), MAX_COMMAND_CHARS + 1);
        assert!(shown.ends_with('…'));
        assert_eq!(shortened("a\tb\nc"), "a b c");
    }
}
