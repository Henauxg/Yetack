//! The words of the app's command line.

/// The word after `flag`: `--demo-app-update offer` gives `offer`. `None` without the flag, or
/// when nothing follows it.
pub fn flag_value<'a>(arguments: &'a [String], flag: &str) -> Option<&'a str> {
    let at = arguments.iter().position(|argument| argument == flag)?;
    arguments.get(at + 1).map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_value_is_the_word_after_the_flag() {
        let arguments: Vec<String> = ["--demo", "--screen", "ready"]
            .iter()
            .map(|&word| word.to_owned())
            .collect();

        assert_eq!(flag_value(&arguments, "--screen"), Some("ready"));
        assert_eq!(flag_value(&arguments, "--demo"), Some("--screen"));
        assert_eq!(flag_value(&arguments, "ready"), None);
        assert_eq!(flag_value(&arguments, "--other"), None);
    }
}
