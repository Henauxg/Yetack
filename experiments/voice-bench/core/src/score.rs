//! Word error rate and term hits, on text normalized so that spoken and written forms compare:
//! "serve dot lock" and "serve.lock", "status five" and "status 5".

/// Words a model may keep or drop without being wrong.
const FILLERS: &[&str] = &["um", "uh", "erm", "hmm", "mm", "ah", "eh"];
/// The spoken names of the symbols that normalization removes.
const SYMBOL_WORDS: &[&str] = &["dot", "slash", "underscore", "dash", "hyphen", "backslash"];

pub fn normalize(text: &str) -> Vec<String> {
    let mut spaced = String::with_capacity(text.len() + 8);
    let mut previous: Option<char> = None;
    for c in text.chars().flat_map(char::to_lowercase) {
        if c == '\'' || c == '\u{2019}' {
            continue;
        }
        if !c.is_alphanumeric() {
            spaced.push(' ');
            previous = None;
            continue;
        }
        if let Some(p) = previous
            && p.is_ascii_digit() != c.is_ascii_digit()
        {
            spaced.push(' ');
        }
        spaced.push(c);
        previous = Some(c);
    }
    let mut words = Vec::new();
    for word in spaced.split_whitespace() {
        if FILLERS.contains(&word) || SYMBOL_WORDS.contains(&word) {
            continue;
        }
        match word.parse::<u64>() {
            Ok(number) if number < 1_000_000 => {
                words.extend(number_words(number).into_iter().map(str::to_owned));
            }
            _ => words.push(word.to_owned()),
        }
    }
    words
}

fn number_words(n: u64) -> Vec<&'static str> {
    const ONES: [&str; 20] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen",
        "nineteen",
    ];
    const TENS: [&str; 10] = [
        "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    let index = |i: u64| usize::try_from(i).unwrap_or(0);
    if n < 20 {
        vec![ONES[index(n)]]
    } else if n < 100 {
        let mut words = vec![TENS[index(n / 10)]];
        if !n.is_multiple_of(10) {
            words.push(ONES[index(n % 10)]);
        }
        words
    } else if n < 1000 {
        let mut words = vec![ONES[index(n / 100)], "hundred"];
        if !n.is_multiple_of(100) {
            words.extend(number_words(n % 100));
        }
        words
    } else {
        let mut words = number_words(n / 1000);
        words.push("thousand");
        if !n.is_multiple_of(1000) {
            words.extend(number_words(n % 1000));
        }
        words
    }
}

/// Word errors (substitutions, insertions and deletions) and the reference's word count.
pub fn word_errors(reference: &str, hypothesis: &str) -> (usize, usize) {
    let reference = normalize(reference);
    let hypothesis = normalize(hypothesis);
    let mut row: Vec<usize> = (0..=hypothesis.len()).collect();
    for (i, r) in reference.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, h) in hypothesis.iter().enumerate() {
            let substitution = diagonal + usize::from(r != h);
            diagonal = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    (row[hypothesis.len()], reference.len())
}

/// Whether the hypothesis holds the term, in any spacing or punctuation.
pub fn has_term(term: &str, hypothesis: &str) -> bool {
    let term = normalize(term).concat();
    !term.is_empty() && normalize(hypothesis).concat().contains(&term)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoken_and_written_forms_match() {
        assert_eq!(word_errors("open serve dot lock", "Open serve.lock."), (0, 3));
        assert_eq!(word_errors("it exits with status five", "It exits with status 5"), (0, 5));
        assert_eq!(word_errors("about two hundred", "About 200,"), (0, 3));
        assert_eq!(word_errors("don't push", "Dont, um, push"), (0, 2));
    }

    #[test]
    fn errors_count_substitutions_insertions_and_deletions() {
        assert_eq!(word_errors("run just check now", "run just a check"), (2, 4));
        assert_eq!(word_errors("a b c", ""), (3, 3));
        assert_eq!(word_errors("", "a"), (1, 0));
    }

    #[test]
    fn terms_match_across_spacing() {
        assert!(has_term("cockpit_daemon", "in the cockpit daemon crate"));
        assert!(has_term("update-daemon.sh", "read update daemon dot sh"));
        assert!(has_term("OSC 52", "osc fifty two"));
        assert!(!has_term("rustls", "rust tls"));
    }
}
