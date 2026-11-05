use ratatui::{
    text::{Line, Span},
    widgets::Block,
};

use crate::ui::text::truncate_spans;

/// Titles `block`, a bordered panel `width` columns wide: `left` from the
/// left corner and `right` (a count or a state) against the right one, as
/// [`fit`] lets them share the top border.
pub fn titled<'a>(
    block: Block<'a>,
    width: u16,
    left: Vec<Span<'a>>,
    right: Vec<Span<'a>>,
) -> Block<'a> {
    // The corners take a column each.
    let (left, right) = fit(left, right, usize::from(width).saturating_sub(2));
    let block = block.title(Line::from(left));
    if right.is_empty() {
        block
    } else {
        block.title(Line::from(right).right_aligned())
    }
}

/// What of a panel's left and right titles fits in `room` columns without
/// one running into the other. Both sides carry their own padding spaces,
/// so they may touch. When they do not fit side by side the right one goes
/// first, then the left one is cut to `…`.
pub fn fit<'a>(
    left: Vec<Span<'a>>,
    right: Vec<Span<'a>>,
    room: usize,
) -> (Vec<Span<'a>>, Vec<Span<'a>>) {
    let width = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    if width(&left) + width(&right) <= room {
        (left, right)
    } else {
        (truncate_spans(left, room), vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    #[test]
    fn the_right_title_goes_before_the_left_one_is_cut() {
        let fitted = |room| {
            let left = vec![Span::raw(" 连接"), Span::raw("  12/30 ")];
            let (left, right) = fit(left, vec![Span::raw(" ● 实时 ")], room);
            (text(&left), text(&right))
        };
        // 13 + 8 columns.
        assert_eq!(
            fitted(21),
            (" 连接  12/30 ".to_owned(), " ● 实时 ".to_owned())
        );
        assert_eq!(fitted(20), (" 连接  12/30 ".to_owned(), String::new()));
        assert_eq!(fitted(13), (" 连接  12/30 ".to_owned(), String::new()));
        assert_eq!(fitted(12), (" 连接  12/3…".to_owned(), String::new()));
        assert_eq!(fitted(1), ("…".to_owned(), String::new()));
        assert_eq!(fitted(0), (String::new(), String::new()));
    }

    #[test]
    fn without_a_right_title_the_left_one_is_cut_only_when_it_overflows() {
        let left = |room| text(&fit(vec![Span::raw(" 规则  6 ")], vec![], room).0);
        // 9 columns.
        assert_eq!(left(9), " 规则  6 ");
        assert_eq!(left(8), " 规则  …");
    }
}
