//! Helpers for the Markdown the bot sends.

/// A fence longer than any backtick run in the text, which can't close it early then.
pub fn code_block(text: &str) -> String {
    let longest = text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}\n{text}\n{fence}")
}

/// The first `max` bytes of `text` at most, cut on a character boundary.
pub fn head(text: &str, max: usize) -> &str {
    &text[..text.floor_char_boundary(max)]
}

/// The last `max` bytes of `text` at most, cut on a character boundary.
pub fn tail(text: &str, max: usize) -> &str {
    &text[text.ceil_char_boundary(text.len().saturating_sub(max))..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_outgrows_backticks_in_the_text() {
        assert_eq!(code_block("a"), "```\na\n```");
        assert_eq!(code_block("a ```` b"), "`````\na ```` b\n`````");
    }

    #[test]
    fn cuts_on_character_boundaries() {
        // "é" is 2 bytes: a cut through its middle drops it whole.
        assert_eq!(head("aéb", 2), "a");
        assert_eq!(tail("aéb", 2), "b");
        assert_eq!(head("ab", 10), "ab");
        assert_eq!(tail("ab", 10), "ab");
    }
}
