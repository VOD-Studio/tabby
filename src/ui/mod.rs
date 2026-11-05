pub mod components;
pub mod theme;

use ratatui::{
    Frame,
    layout::{Constraint, Layout},
};

use crate::app::{self, App};

pub fn draw(frame: &mut Frame, app: &App) {
    let [header, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(frame.area());
    components::header::render(frame, header, app);
    app::draw(frame, body, app);
}
