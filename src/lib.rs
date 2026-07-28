pub mod actions;
pub mod bindings;
pub mod settings;

mod backend;
mod font;
mod terminal;
mod theme;
mod view;

pub use rio_vt::crosswords::pos::Pos as TerminalPoint;
pub use rio_vt::crosswords::Mode as TermMode;
pub use rio_vt::event::RioEvent;
pub use rio_vt::selection::SelectionType;
pub use backend::Command as BackendCommand;
pub use backend::{LinkAction, MouseButton};
pub use terminal::{Command, Event, Terminal};
pub use theme::{ColorPalette, Theme};
pub use view::TerminalView;
