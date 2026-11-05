use std::{cell::Cell, ops::Range};

/// Where a scrolling list starts. The cursor moves inside the visible window
/// and the window only follows once the cursor would leave it.
///
/// The offset sits in a `Cell` because it is settled while drawing, when the
/// viewport size is first known.
#[derive(Debug, Default)]
pub struct Scroll {
    offset: Cell<usize>,
}

impl Scroll {
    /// The indices of the `len` items to draw into a viewport that fits
    /// `capacity` of them, keeping `selected` in view.
    pub fn window(&self, selected: usize, len: usize, capacity: usize) -> Range<usize> {
        let capacity = capacity.max(1);
        let mut offset = self.offset.get().min(len.saturating_sub(capacity));
        if selected < offset {
            offset = selected;
        } else if selected >= offset + capacity {
            offset = selected + 1 - capacity;
        }
        self.offset.set(offset);
        offset..(offset + capacity).min(len)
    }
}
