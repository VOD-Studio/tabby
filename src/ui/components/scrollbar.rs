//! A scrollbar drawn over the right border of a panel: the border is the
//! track, a block on it the thumb. Pressing on the track or dragging along
//! it scrolls there (`App::mouse_action`).

use std::ops::Range;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
};

use crate::ui::hit::{Hits, Target};

/// The thumb. A block character: the same in both icon sets.
const THUMB: &str = "█";

/// The cells of a track `track` long the thumb covers, for a view of
/// `viewport` rows starting at row `position` of `len`. Its size is the
/// share of `len` in view. It touches the top only at the first position
/// and the bottom only at the last, so a glance tells whether there is
/// more. `None` when everything fits, or the track is too short for a
/// thumb that can move.
pub fn thumb(track: usize, position: usize, viewport: usize, len: usize) -> Option<Range<usize>> {
    if len <= viewport || track < 2 {
        return None;
    }
    let last = len - viewport;
    let position = position.min(last);
    let size = ((track * viewport + len / 2) / len).clamp(1, track - 1);
    let free = track - size;
    let mut start = (position * free + last / 2) / last;
    if position > 0 && position < last && free >= 2 {
        start = start.clamp(1, free - 1);
    }
    Some(start..start + size)
}

/// Draws the thumb, in `color`, on the right border of a panel, alongside
/// `rows`: the rows of the panel's inside that scroll, a table's header
/// left out. The border is the column just right of them, as with a
/// bordered block without padding. `position`, `viewport` and `len` as in
/// [`thumb`]; nothing is drawn when everything fits. The track goes into
/// `hits` for the mouse.
pub fn render(
    frame: &mut Frame,
    hits: &Hits,
    rows: Rect,
    position: usize,
    viewport: usize,
    len: usize,
    color: Color,
) {
    let Some(thumb) = thumb(usize::from(rows.height), position, viewport, len) else {
        return;
    };
    let x = rows.right();
    let track = Rect {
        x,
        width: 1,
        ..rows
    };
    hits.add(track, Target::Track(track));
    let buffer = frame.buffer_mut();
    for row in (0..rows.height).filter(|row| thumb.contains(&usize::from(*row))) {
        if let Some(cell) = buffer.cell_mut((x, rows.y + row)) {
            cell.set_symbol(THUMB).set_style(Style::new().fg(color));
        }
    }
}

/// The rows of a table's inside `inner` under its one header row.
pub fn under_header(inner: Rect) -> Rect {
    Rect {
        y: inner.y.saturating_add(1),
        height: inner.height.saturating_sub(1),
        ..inner
    }
}

/// The rows of a popup's inside `inner`, as `popup::panel` gives it: it
/// is padded by a column, so its border is one further right.
pub fn in_popup(inner: Rect) -> Rect {
    Rect {
        width: inner.width.saturating_add(1),
        ..inner
    }
}

/// [`render`] for a list scrolled to `window` (as `Scroll::window` gives
/// it) of `len` items, one row each.
pub fn list(
    frame: &mut Frame,
    hits: &Hits,
    rows: Rect,
    window: &Range<usize>,
    len: usize,
    color: Color,
) {
    render(frame, hits, rows, window.start, window.len(), len, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_shows_when_everything_fits_or_the_track_cannot_move() {
        assert_eq!(thumb(10, 0, 10, 10), None);
        assert_eq!(thumb(10, 0, 10, 3), None);
        assert_eq!(thumb(1, 0, 1, 50), None);
        assert_eq!(thumb(0, 0, 0, 50), None);
    }

    #[test]
    fn the_thumb_is_the_share_in_view_and_never_the_whole_track() {
        assert_eq!(thumb(10, 0, 5, 10), Some(0..5));
        assert_eq!(thumb(10, 0, 1, 2000), Some(0..1));
        // 19 of 20 in view still leaves room to move.
        assert_eq!(thumb(10, 0, 19, 20), Some(0..9));
    }

    #[test]
    fn only_the_first_and_last_positions_touch_the_ends() {
        let (track, viewport, len) = (20, 40, 2000);
        let last = len - viewport;
        let size = thumb(track, 0, viewport, len).unwrap().len();
        assert_eq!(thumb(track, 0, viewport, len), Some(0..size));
        assert_eq!(thumb(track, last, viewport, len), Some(track - size..track));
        // Past the end counts as the end.
        assert_eq!(
            thumb(track, last + 7, viewport, len),
            Some(track - size..track)
        );
        // One row off either end shows as off it.
        assert_eq!(thumb(track, 1, viewport, len).unwrap().start, 1);
        assert_eq!(
            thumb(track, last - 1, viewport, len).unwrap().end,
            track - 1
        );
    }

    #[test]
    fn the_thumb_moves_down_as_the_view_does_and_keeps_its_size() {
        let (track, viewport, len) = (15, 30, 333);
        let size = thumb(track, 0, viewport, len).unwrap().len();
        let mut previous = 0;
        for position in 0..=len - viewport {
            let range = thumb(track, position, viewport, len).unwrap();
            assert_eq!(range.len(), size);
            assert!(range.end <= track);
            assert!(range.start >= previous, "at {position}");
            previous = range.start;
        }
        assert_eq!(previous, track - size);
    }
}
