use std::cmp::Ordering;

use jiff::{Timestamp, Zoned};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const ELLIPSIS: char = '…';

/// Shortens `text` to at most `max_width` terminal columns, ending in `…` when
/// anything was cut. Wide characters (CJK, emoji) are never split, and
/// neither is a grapheme: half a flag is a lone regional indicator, one
/// column to us and two to many terminals.
pub fn truncate(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_owned();
    }

    // One column is reserved for the ellipsis.
    let budget = max_width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let w = grapheme.width();
        if used + w > budget {
            break;
        }
        used += w;
        out.push_str(grapheme);
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

/// How long something has lasted, in the two leading units: `45s`, `12m05s`,
/// `3h05m`, `2d03h`. Seconds are dropped once there are hours, as they would
/// only flicker.
pub fn duration(secs: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match secs {
        0..MINUTE => format!("{secs}s"),
        MINUTE..HOUR => format!("{}m{:02}s", secs / MINUTE, secs % MINUTE),
        HOUR..DAY => format!("{}h{:02}m", secs / HOUR, secs % HOUR / MINUTE),
        _ => format!("{}d{:02}h", secs / DAY, secs % DAY / HOUR),
    }
}

/// How long ago `then` was, as of `now`: `刚刚` under a minute, then
/// `N 分钟前`, `N 小时前`, `N 天前` up to 30 days, and past that the date
/// in `now`'s time zone (`2026-10-01`). A time in the future, which only a
/// clock running ahead of the kernel's gives, counts as `刚刚`.
pub fn ago(then: Timestamp, now: &Zoned) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let secs = now.timestamp().as_second() - then.as_second();
    match secs {
        ..MINUTE => "刚刚".to_owned(),
        MINUTE..HOUR => format!("{} 分钟前", secs / MINUTE),
        HOUR..DAY => format!("{} 小时前", secs / HOUR),
        _ if secs / DAY <= 30 => format!("{} 天前", secs / DAY),
        _ => then.to_zoned(now.time_zone().clone()).date().to_string(),
    }
}

/// Units of a transfer rate, 1024 apart. The only place unit names live:
/// plain byte sizes use them without the `/s`.
const RATE_UNITS: [&str; 4] = ["B/s", "KB/s", "MB/s", "GB/s"];

/// A transfer rate as a number and its unit: `("1.0", "KB/s")`.
///
/// Whole bytes show no decimal, everything above has one. Kept apart so the
/// caller can color them differently.
pub fn rate(bytes_per_second: u64) -> (String, &'static str) {
    let (number, unit) = scale(bytes_per_second);
    (number, RATE_UNITS[unit])
}

/// A byte count as a number and its unit: `("1.8", "GB")`, `("1023", "B")`.
/// Rounds and carries like [`rate`].
pub fn bytes(count: u64) -> (String, &'static str) {
    let (number, unit) = scale(count);
    (number, unit_name(unit))
}

/// The largest unit `bytes` reaches, as its size in bytes and its name
/// (`(1 << 20, "MB")`); anything under 1 KB is in bytes. Unlike [`rate`] this
/// does not round up into the next unit: the axis labels of a chart count in
/// whole units of it.
pub fn unit_for(bytes: u64) -> (u64, &'static str) {
    let index = (0..RATE_UNITS.len())
        .rev()
        .find(|&index| bytes >> (10 * index) > 0)
        .unwrap_or(0);
    (1 << (10 * index), unit_name(index))
}

/// The name of unit `index` without the rate's `/s`.
fn unit_name(index: usize) -> &'static str {
    RATE_UNITS[index].trim_end_matches("/s")
}

/// Scales `bytes` down to the largest unit that keeps the number below 1024,
/// returning it and the unit's index (0 = B, 3 = GB).
///
/// Works on tenths with integer rounding: rounding 1023.96 KB through a float
/// would print `1024.0 KB` instead of moving on to `1.0 MB`.
fn scale(bytes: u64) -> (String, usize) {
    if bytes < 1024 {
        return (bytes.to_string(), 0);
    }
    let bytes = u128::from(bytes);
    let mut unit = 1;
    let mut divisor = 1024;
    let tenths = |divisor: u128| (bytes * 10 + divisor / 2) / divisor;
    while unit < RATE_UNITS.len() - 1 && tenths(divisor) >= 10240 {
        unit += 1;
        divisor *= 1024;
    }
    let tenths = tenths(divisor);
    (format!("{}.{}", tenths / 10, tenths % 10), unit)
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
    fn never_splits_a_flag_or_undercounts_an_emoji_sequence() {
        // Per char, `🇭` alone is one column and would fit before the `…`.
        assert_eq!(truncate("ab🇭🇰cd", 4), "ab…");
        assert_eq!(truncate("ab🇭🇰cd", 5), "ab🇭🇰…");
        // `☁️` is two columns: `a☁️b` is 4 wide and does not fit 3.
        assert_eq!(truncate("a☁️b", 3), "a…");
        assert_eq!(truncate("a☁️b", 4), "a☁️b");
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

    fn shown(bytes_per_second: u64) -> String {
        let (number, unit) = rate(bytes_per_second);
        format!("{number} {unit}")
    }

    #[test]
    fn whole_bytes_stay_bytes_until_1024() {
        assert_eq!(shown(0), "0 B/s");
        assert_eq!(shown(1023), "1023 B/s");
        assert_eq!(shown(1024), "1.0 KB/s");
        assert_eq!(shown(1536), "1.5 KB/s");
    }

    #[test]
    fn each_unit_rolls_over_at_the_next_power_of_1024() {
        assert_eq!(shown(1024 * 1024), "1.0 MB/s");
        assert_eq!(shown(1024 * 1024 * 1024), "1.0 GB/s");
        assert_eq!(shown(5 * 1024 * 1024 + 512 * 1024), "5.5 MB/s");
    }

    #[test]
    fn rounding_up_to_1024_carries_into_the_next_unit() {
        // 1023.949… KB rounds down, 1023.950… KB rounds up past the limit.
        assert_eq!(shown(1024 * 1024 - 52), "1023.9 KB/s");
        assert_eq!(shown(1024 * 1024 - 51), "1.0 MB/s");
        assert_eq!(shown(1024 * 1024 * 1024 - 1), "1.0 GB/s");
    }

    #[test]
    fn gigabytes_do_not_roll_over_and_the_largest_rate_does_not_overflow() {
        assert_eq!(shown(2048 * 1024 * 1024 * 1024), "2048.0 GB/s");
        assert!(shown(u64::MAX).ends_with("GB/s"));
    }

    fn size(count: u64) -> String {
        let (number, unit) = bytes(count);
        format!("{number} {unit}")
    }

    #[test]
    fn sizes_have_the_rates_numbers_without_the_per_second() {
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1024), "1.0 KB");
        // Rounding up to 1024 carries into the next unit, as it does for rates.
        assert_eq!(size(1024 * 1024 - 51), "1.0 MB");
        assert_eq!(size(1_954_000_000), "1.8 GB");
        for count in [0, 1023, 1 << 10, 1 << 20, 1 << 30, u64::MAX] {
            let (_, size_unit) = bytes(count);
            let (_, rate_unit) = rate(count);
            assert_eq!(format!("{size_unit}/s"), rate_unit, "{count}");
        }
    }

    #[test]
    fn durations_keep_the_two_leading_units_and_roll_over_at_the_boundaries() {
        for (secs, text) in [
            (0, "0s"),
            (59, "59s"),
            (60, "1m00s"),
            (725, "12m05s"),
            (3599, "59m59s"),
            (3600, "1h00m"),
            (3 * 3600 + 5 * 60 + 59, "3h05m"),
            (86_399, "23h59m"),
            (86_400, "1d00h"),
            (2 * 86_400 + 3 * 3600, "2d03h"),
        ] {
            assert_eq!(duration(secs), text, "{secs}");
        }
    }

    #[test]
    fn relative_times_switch_units_at_the_boundaries() {
        let now: Zoned = "2026-11-05T12:00:00+08:00[Asia/Shanghai]".parse().unwrap();
        let ago_secs = |secs: i64| {
            let then = now.timestamp() - jiff::SignedDuration::from_secs(secs);
            ago(then, &now)
        };
        for (secs, text) in [
            (-3600, "刚刚"),
            (0, "刚刚"),
            (59, "刚刚"),
            (60, "1 分钟前"),
            (59 * 60 + 59, "59 分钟前"),
            (3600, "1 小时前"),
            (23 * 3600 + 59 * 60, "23 小时前"),
            (86_400, "1 天前"),
            (30 * 86_400, "30 天前"),
            (30 * 86_400 + 86_399, "30 天前"),
            (31 * 86_400, "2026-10-05"),
        ] {
            assert_eq!(ago_secs(secs), text, "{secs}");
        }
        // The date is the one in `now`'s time zone: 23:30 UTC is the next
        // day in Shanghai.
        let then: Timestamp = "2026-09-30T23:30:00Z".parse().unwrap();
        assert_eq!(ago(then, &now), "2026-10-01");
    }
}
