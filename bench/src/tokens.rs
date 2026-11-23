//! One token counter for every tool, so budgets mean the same thing.

/// `ceil(chars / 4)`: the common rule of thumb, deliberately simple and the
/// same for every adapter. Tool-reported counts are never used.
#[must_use]
pub fn est(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::est;

    #[test]
    fn rounds_up_by_chars_not_bytes() {
        assert_eq!(est(""), 0);
        assert_eq!(est("abcd"), 1);
        assert_eq!(est("abcde"), 2);
        assert_eq!(est("ééééé"), 2);
    }
}
