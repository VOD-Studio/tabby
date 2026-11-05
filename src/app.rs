use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

/// A user intent, derived from a terminal event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Resize,
}

/// A message from a background task. No task exists yet, so no variant does.
#[derive(Debug)]
pub enum Msg {}

/// Maps a terminal event to an [`Action`]; `None` means the event is ignored.
pub fn action_for(event: &Event) -> Option<Action> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            let ctrl_c =
                key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
            (ctrl_c || key.code == KeyCode::Char('q')).then_some(Action::Quit)
        }
        Event::Resize(..) => Some(Action::Resize),
        _ => None,
    }
}

/// Application state. Only the main loop mutates it.
#[derive(Debug, Default)]
pub struct App {
    should_quit: bool,
    dirty: bool,
}

impl App {
    pub fn update(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::Resize => self.dirty = true,
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Returns whether a repaint is pending, and clears the flag.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
}
