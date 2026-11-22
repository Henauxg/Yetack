/// The sounds a speaker makes while thinking, which the model writes out as words.
const FILLERS: [&str; 7] = ["um", "umm", "uhm", "uh", "uhh", "erm", "hmm"];

/// `text` without its filler words. A dropped filler that began a sentence passes its capital to
/// the next word, and its full stop to the word before it.
pub(crate) fn without_fillers(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    push_without_fillers(&mut kept, text);
    kept
}

/// Adds `text` without its filler words after `kept`, with a space, as [`without_fillers`] gives
/// it. The end of `kept` counts as the words before `text`: it takes the full stop of a filler
/// that starts `text`.
pub(crate) fn push_without_fillers(kept: &mut String, text: &str) {
    let mut capital = false;
    for word in text.split_whitespace() {
        let letters = word.trim_matches(|c: char| !c.is_alphanumeric());
        if FILLERS
            .iter()
            .any(|filler| filler.eq_ignore_ascii_case(letters))
        {
            capital |= ends_sentence(kept) && letters.starts_with(char::is_uppercase);
            if let Some(stop) = word.chars().last().filter(|&c| is_stop(c))
                && !kept.is_empty()
            {
                if kept.ends_with(',') {
                    kept.pop();
                }
                if !ends_sentence(kept) {
                    kept.push(stop);
                }
            }
            continue;
        }
        if !kept.is_empty() {
            kept.push(' ');
        }
        let mut chars = word.chars();
        if capital && let Some(first) = chars.next() {
            kept.extend(first.to_uppercase());
            kept.push_str(chars.as_str());
        } else {
            kept.push_str(word);
        }
        capital = false;
    }
}

fn ends_sentence(text: &str) -> bool {
    text.chars().last().is_none_or(is_stop)
}

fn is_stop(c: char) -> bool {
    matches!(c, '.' | '!' | '?')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fillers_go_and_the_words_around_them_stay() {
        assert_eq!(
            without_fillers("Open the um config file and uh check the port."),
            "Open the config file and check the port."
        );
        assert_eq!(
            without_fillers("Then Uh rename it, hmm, to main."),
            "Then rename it, to main."
        );
        assert_eq!(without_fillers("Run the tests"), "Run the tests");
    }

    #[test]
    fn a_filler_that_starts_a_sentence_gives_the_next_word_its_capital() {
        assert_eq!(
            without_fillers("Um, so we start. Uh then we stop."),
            "So we start. Then we stop."
        );
        assert_eq!(without_fillers("Done. um next one"), "Done. next one");
    }

    #[test]
    fn a_filler_that_ends_a_sentence_leaves_its_stop() {
        assert_eq!(without_fillers("Push it, uh."), "Push it.");
        assert_eq!(without_fillers("Is it green um?"), "Is it green?");
        assert_eq!(without_fillers("It works. Um."), "It works.");
    }

    #[test]
    fn only_fillers_leave_nothing() {
        assert_eq!(without_fillers("Um. Uh, hmm"), "");
        assert_eq!(without_fillers(""), "");
    }

    #[test]
    fn words_that_contain_a_filler_stay() {
        assert_eq!(
            without_fillers("Huh, the umbrella is hummed uh-huh"),
            "Huh, the umbrella is hummed uh-huh"
        );
    }
}
