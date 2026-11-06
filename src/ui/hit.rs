use std::cell::RefCell;

use ratatui::layout::{Position, Rect};

use crate::app::{Page, Pane, ProviderRef, Setting};

/// What a place on the screen stands for when the mouse acts on it.
///
/// Items that come and go with the data are named by what they are, not by
/// where they stood when drawn: the screen and the click can be a tick
/// apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A tab of the page row.
    Tab(Page),
    /// A pane of the proxies page, for the wheel.
    Pane(Pane),
    /// The list of any other page, for the wheel.
    List,
    /// Everything under a popup: a click there dismisses it.
    Backdrop,
    /// A popup's own face: a click there goes nowhere.
    Popup,
    /// The group a row of the proxies page's group list is for.
    Group(String),
    /// The narrow proxies page's group picker row.
    GroupPicker,
    /// A row of the picker's drop-down, by group.
    PickerItem(String),
    /// A node card.
    Node(String),
    /// A row of the connections table, by connection id.
    Connection(String),
    /// A row of the rules table, by its number.
    Rule(usize),
    Provider(ProviderRef),
    Setting(Setting),
    /// A row of the profile popup, by index.
    Profile(usize),
    /// A row of the theme picker, by index.
    Theme(usize),
}

impl Target {
    /// Whether the wheel scrolls it.
    fn scrolls(&self) -> bool {
        matches!(self, Self::Pane(_) | Self::List)
    }

    /// Whether a second press in quick succession acts on it, as `Enter`
    /// does, instead of only picking it.
    pub fn activatable(&self) -> bool {
        matches!(
            self,
            Self::Group(_)
                | Self::PickerItem(_)
                | Self::Node(_)
                | Self::Connection(_)
                | Self::Rule(_)
                | Self::Provider(_)
                | Self::Setting(_)
                | Self::Profile(_)
                | Self::Theme(_)
        )
    }
}

/// What the last frame drew that the mouse can act on, filled in while
/// drawing: the layout is only known there. Later entries lie over earlier
/// ones.
///
/// Like [`Scroll`](crate::ui::scroll::Scroll) it is written through a shared
/// reference, as drawing only borrows the app.
#[derive(Debug, Default)]
pub struct Hits {
    areas: RefCell<Vec<(Rect, Target)>>,
}

impl Hits {
    /// Forgets everything; a frame starts with this.
    pub fn clear(&self) {
        self.areas.borrow_mut().clear();
    }

    /// Registers `area` over what is registered so far. An empty area has
    /// nothing to click on.
    pub fn add(&self, area: Rect, target: Target) {
        if !area.is_empty() {
            self.areas.borrow_mut().push((area, target));
        }
    }

    /// What is on top at `position`.
    pub fn at(&self, position: Position) -> Option<Target> {
        self.find(position, |_| true)
    }

    /// The topmost thing at `position` the wheel scrolls.
    pub fn scroll_area(&self, position: Position) -> Option<Target> {
        self.find(position, Target::scrolls)
    }

    fn find(&self, position: Position, wanted: impl Fn(&Target) -> bool) -> Option<Target> {
        self.areas
            .borrow()
            .iter()
            .rev()
            .find(|(area, target)| area.contains(position) && wanted(target))
            .map(|(_, target)| target.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_registered_area_is_on_top_and_empty_ones_are_ignored() {
        let hits = Hits::default();
        hits.add(Rect::new(0, 0, 10, 10), Target::Backdrop);
        hits.add(Rect::new(2, 2, 4, 4), Target::Popup);
        hits.add(Rect::new(3, 3, 0, 5), Target::List);
        assert_eq!(hits.at(Position::new(3, 3)), Some(Target::Popup));
        assert_eq!(hits.at(Position::new(8, 8)), Some(Target::Backdrop));
        assert_eq!(hits.at(Position::new(10, 10)), None);
        hits.clear();
        assert_eq!(hits.at(Position::new(3, 3)), None);
    }

    #[test]
    fn the_wheel_looks_through_what_it_does_not_scroll() {
        let hits = Hits::default();
        hits.add(Rect::new(0, 0, 10, 10), Target::List);
        hits.add(Rect::new(2, 2, 4, 4), Target::Tab(Page::Rules));
        assert_eq!(hits.scroll_area(Position::new(3, 3)), Some(Target::List));
        assert_eq!(hits.at(Position::new(3, 3)), Some(Target::Tab(Page::Rules)));
    }
}
