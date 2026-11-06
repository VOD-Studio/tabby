//! Repaints cells a terminal blanks when a wide character is cut in half.
//!
//! A terminal that prints over one column of a wide character erases its
//! other columns too. Ratatui's diff skips the columns a new wide character
//! covers, so when `宽` lands on 8–9 where `节` sat on 9–10, column 10 is
//! erased on screen but equal in both buffers and never sent again: on a
//! reversed cursor row it stays a default-colored hole until the next full
//! repaint. Scrolling a list of CJK and emoji names does this on every step.

use ratatui::buffer::{Buffer, CellDiffOption, CellWidth};

/// The last frame sent to the terminal, to find the cells the next one cuts.
#[derive(Debug, Default)]
pub struct Shadow {
    last: Buffer,
}

impl Shadow {
    /// Marks every cell of `next` that the terminal will blank while drawing
    /// over the last frame, so the diff sends it again.
    pub fn mark(&self, next: &mut Buffer) {
        let last = &self.last;
        if last.area != next.area {
            // A resize clears the screen: no old wide character is left.
            return;
        }
        let width = usize::from(next.area.width);
        for row in 0..usize::from(next.area.height) {
            let start = row * width;
            let mut x = 0;
            while x < width {
                let cell = &next.content[start + x];
                let span = usize::from(cell.symbol().cell_width()).max(1);
                let sent = match cell.diff_option {
                    CellDiffOption::Skip => false,
                    CellDiffOption::AlwaysUpdate => true,
                    _ => *cell != last.content[start + x],
                };
                if sent {
                    // A wide character of the last frame starting under this
                    // write and reaching past it loses its overhang.
                    let end = x + span;
                    let overhang = (x..end.min(width))
                        .map(|q| q + usize::from(last.content[start + q].symbol().cell_width()))
                        .max()
                        .unwrap_or(end)
                        .min(width);
                    for cut in end..overhang {
                        next.content[start + cut].set_diff_option(CellDiffOption::AlwaysUpdate);
                    }
                }
                x += span;
            }
        }
    }

    /// Records `drawn` as what the terminal now shows.
    pub fn remember(&mut self, drawn: &Buffer) {
        self.last.area = drawn.area;
        self.last.content.clone_from(&drawn.content);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{
        layout::Rect,
        style::{Modifier, Style},
    };

    use super::*;

    /// A reversed cursor row reading `text`, like the selected group.
    fn row(text: &str) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        buf.set_string(0, 0, text, Style::new());
        buf.set_style(buf.area, Style::new().add_modifier(Modifier::REVERSED));
        buf
    }

    fn sent(last: &Buffer, next: &Buffer) -> Vec<u16> {
        last.diff(next).into_iter().map(|(x, _, _)| x).collect()
    }

    #[test]
    fn a_wide_character_cut_in_half_is_repainted() {
        // `节` sits on 9–10; `宽` takes 8–9, so the screen erases 10.
        let last = row("→ 🎬 解锁节点");
        let mut next = row("→ 苏菲家宽");
        assert!(!sent(&last, &next).contains(&10));

        let mut shadow = Shadow::default();
        shadow.remember(&last);
        shadow.mark(&mut next);
        assert!(sent(&last, &next).contains(&10));
    }

    #[test]
    fn aligned_wide_characters_send_nothing_extra() {
        // Every old wide character starts under a new one: nothing is cut.
        let last = row("→ 解锁节点");
        let mut next = row("→ 苏菲家宽");
        let plain = sent(&last, &next);

        let mut shadow = Shadow::default();
        shadow.remember(&last);
        shadow.mark(&mut next);
        assert_eq!(sent(&last, &next), plain);
    }
}
