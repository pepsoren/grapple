//! Name normalisation shared by the scanner and manual entries.

/// Lowercased name for sorting and search: punctuation and symbols are dropped (`'`, `.`, `™`, `®`, `©`
/// disappear, other punctuation becomes a space), whitespace is collapsed and a leading "the " is removed.
///
/// `"The Witcher 3: Wild Hunt"` -> `"witcher 3 wild hunt"`, `"Rocket League®"` -> `"rocket league"`.
pub fn sort_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_space = false;
    for ch in name.chars() {
        if matches!(ch, '\'' | '’' | '‘' | '.' | '™' | '®' | '©') {
            continue;
        }
        if ch.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.extend(ch.to_lowercase());
        } else {
            pending_space = true;
        }
    }
    if out.is_empty() {
        // A name made only of symbols: keep it searchable rather than empty.
        return name.trim().to_lowercase();
    }
    match out.strip_prefix("the ") {
        Some(rest) if !rest.is_empty() => rest.to_string(),
        _ => out,
    }
}

#[cfg(test)]
mod tests {
    use super::sort_name;

    #[test]
    fn strips_articles_and_punctuation() {
        assert_eq!(sort_name("The Witcher 3: Wild Hunt"), "witcher 3 wild hunt");
        assert_eq!(sort_name("Rocket League®"), "rocket league");
        assert_eq!(sort_name("Dr. Mario's  Lab"), "dr marios lab");
        assert_eq!(sort_name("Half-Life 2"), "half life 2");
        assert_eq!(sort_name("S.T.A.L.K.E.R."), "stalker");
        assert_eq!(sort_name("  Theme Hospital "), "theme hospital");
        assert_eq!(sort_name("The"), "the");
        assert_eq!(sort_name("???"), "???");
        assert_eq!(sort_name("Ōkami HD"), "ōkami hd");
    }
}
