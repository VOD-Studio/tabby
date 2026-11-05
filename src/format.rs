use std::cmp::Ordering;

use unicode_width::UnicodeWidthChar;

const ELLIPSIS: char = '…';

/// Shortens `text` to at most `max_width` terminal columns, ending in `…` when
/// anything was cut. Wide characters (CJK, emoji) are never split.
pub fn truncate(text: &str, max_width: usize) -> String {
    let mut width = 0;
    let mut fits = true;
    for ch in text.chars() {
        width += ch.width().unwrap_or(0);
        if width > max_width {
            fits = false;
            break;
        }
    }
    if fits {
        return text.to_owned();
    }

    // One column is reserved for the ellipsis.
    let budget = max_width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(ch);
    }
    if max_width > 0 {
        out.push(ELLIPSIS);
    }
    out
}

/// Orders `a` and `b` char by char, except that a run of ASCII digits on both
/// sides compares as a number: `"2" < "10"`, `"07" == "7"`. Everything else
/// compares by Unicode scalar value, so callers that want case-insensitive
/// order lowercase both sides first.
pub fn natural_cmp(mut a: &str, mut b: &str) -> Ordering {
    loop {
        let (Some(x), Some(y)) = (a.chars().next(), b.chars().next()) else {
            // One side ran out (or both): the shorter remainder sorts first.
            return a.len().cmp(&b.len());
        };
        if x.is_ascii_digit() && y.is_ascii_digit() {
            let (digits_a, rest_a) = split_digits(a);
            let (digits_b, rest_b) = split_digits(b);
            match cmp_numbers(digits_a, digits_b) {
                Ordering::Equal => (a, b) = (rest_a, rest_b),
                other => return other,
            }
        } else {
            match x.cmp(&y) {
                Ordering::Equal => (a, b) = (&a[x.len_utf8()..], &b[y.len_utf8()..]),
                other => return other,
            }
        }
    }
}

/// Splits off the leading run of ASCII digits.
fn split_digits(text: &str) -> (&str, &str) {
    text.split_at(
        text.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len()),
    )
}

/// Compares two digit runs by value, however long they are.
fn cmp_numbers(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::*;

    #[test]
    fn text_that_fits_is_unchanged() {
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("你好", 4), "你好");
    }

    #[test]
    fn cuts_ascii_with_ellipsis_inside_the_limit() {
        assert_eq!(truncate("hello world", 6), "hello…");
    }

    #[test]
    fn never_splits_wide_characters() {
        // "你好" is 4 columns; one more would overflow 5 once the ellipsis is added.
        assert_eq!(truncate("你好世界", 5), "你好…");
        // Even limit: the third character would straddle the boundary.
        assert_eq!(truncate("你好世界", 4), "你…");
        assert_eq!(truncate("a😀bc", 3), "a…");
        assert_eq!(truncate("a😀bc", 4), "a😀…");
    }

    #[test]
    fn result_never_exceeds_the_limit() {
        for text in ["香港 01 🇭🇰 Trojan", "plain ascii name", "😀😀😀😀"] {
            for limit in 0..20 {
                assert!(
                    truncate(text, limit).width() <= limit,
                    "{text:?} at {limit}"
                );
            }
        }
    }
}
