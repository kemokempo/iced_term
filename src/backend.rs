use crate::actions::Action;
use crate::settings::BackendSettings;
use corcovado::channel::Sender;
use iced::keyboard::Modifiers;
use iced_core::Size;
use rio_vt::ansi::CursorShape;
use rio_vt::config::colors::{AnsiColor, ColorRgb, NamedColor};
use rio_vt::crosswords::grid::{Dimensions, Grid, Scroll};
use rio_vt::crosswords::pos::{Column, Direction, Line, Pos, Side};
use rio_vt::crosswords::search::{Match, RegexIter, RegexSearch};
use rio_vt::crosswords::square::{ContentTag, Square};
use rio_vt::crosswords::{Crosswords, Mode as TermMode};
use rio_vt::event::sync::FairMutex;
use rio_vt::event::{EventListener, Msg, RioEvent, WindowId};
use rio_vt::performer::Machine;
use rio_vt::selection::{Selection, SelectionRange, SelectionType};
use std::borrow::Cow;
use std::cmp::min;
use std::io::Result;
use std::ops::RangeInclusive;
use std::sync::Arc;
use teletypewriter::{create_pty_with_spawn, WinsizeBuilder};
use tokio::sync::mpsc;

const URL_REGEX: &str = r#"(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file://|git://|ssh:|ftp://)[^\u{0000}-\u{001F}\u{007F}-\u{009F}<>"\s{-}\^⟨⟩`]+"#;

const SCROLLBACK_HISTORY: usize = 10_000;

#[derive(Debug, Clone)]
pub enum Command {
    Write(Vec<u8>),
    Scroll(i32),
    Resize(Option<Size<f32>>, Option<Size<f32>>),
    SelectStart(SelectionType, (f32, f32)),
    SelectUpdate((f32, f32)),
    ProcessLink(LinkAction, Pos),
    MouseReport(MouseButton, Modifiers, Pos, bool),
    ProcessRioEvent(RioEvent),
}

#[derive(Debug, Clone)]
pub enum MouseMode {
    Sgr,
    Normal(bool),
}

impl From<TermMode> for MouseMode {
    fn from(term_mode: TermMode) -> Self {
        if term_mode.contains(TermMode::SGR_MOUSE) {
            MouseMode::Sgr
        } else if term_mode.contains(TermMode::UTF8_MOUSE) {
            MouseMode::Normal(true)
        } else {
            MouseMode::Normal(false)
        }
    }
}

#[derive(Debug, Clone)]
pub enum MouseButton {
    LeftButton = 0,
    MiddleButton = 1,
    RightButton = 2,
    LeftMove = 32,
    MiddleMove = 33,
    RightMove = 34,
    NoneMove = 35,
    ScrollUp = 64,
    ScrollDown = 65,
    Other = 99,
}

#[derive(Debug, Clone)]
pub enum LinkAction {
    Clear,
    Hover,
    Open,
}

#[derive(Clone, Copy, Debug)]
pub struct TerminalSize {
    pub cell_width: u16,
    pub cell_height: u16,
    num_cols: u16,
    num_lines: u16,
    layout_width: f32,
    layout_height: f32,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cell_width: 1,
            cell_height: 1,
            num_cols: 80,
            num_lines: 50,
            layout_width: 80.0,
            layout_height: 50.0,
        }
    }
}

impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn columns(&self) -> usize {
        self.num_cols as usize
    }

    fn last_column(&self) -> Column {
        Column(self.num_cols as usize - 1)
    }

    fn bottommost_line(&self) -> Line {
        Line(self.num_lines as i32 - 1)
    }

    fn screen_lines(&self) -> usize {
        self.num_lines as usize
    }
}

pub struct Backend {
    term: Arc<FairMutex<Crosswords<EventProxy>>>,
    size: TerminalSize,
    channel: Sender<Msg>,
    last_content: RenderableContent,
    pub(crate) url_regex: RegexSearch,
}

impl Backend {
    pub fn new(
        id: u64,
        pty_event_proxy_sender: mpsc::Sender<RioEvent>,
        settings: BackendSettings,
    ) -> Result<Self> {
        let terminal_size = TerminalSize::default();
        let event_proxy = EventProxy(pty_event_proxy_sender);

        let mut term = Crosswords::new(
            terminal_size,
            CursorShape::Block,
            event_proxy.clone(),
            WindowId::from(id),
            0,
            SCROLLBACK_HISTORY,
        );

        let cursor = *term.grid.cursor_cell();

        let initial_content = RenderableContent {
            grid: term.grid.clone(),
            selectable_range: None,
            terminal_mode: term.mode(),
            terminal_size,
            cursor,
            hovered_hyperlink: None,
        };

        let term = Arc::new(FairMutex::new(term));

        // rio-vt's teletypewriter spawns the PTY directly.
        // TODO(rio-vt): create_pty_with_spawn takes no env map, so
        // BackendSettings.env is not applied. The alacritty backend passed
        // env through tty::Options. Wire this once teletypewriter grows an
        // env parameter (or set it on the spawned child another way).
        let working_directory = settings
            .working_directory
            .as_ref()
            .map(|path| path.to_string_lossy().to_string());

        let pty = create_pty_with_spawn(
            &settings.program,
            settings.args.clone(),
            &working_directory,
            terminal_size.num_cols,
            terminal_size.num_lines,
            0,
            0,
        )?;

        let machine = Machine::new(
            Arc::clone(&term),
            pty,
            event_proxy,
            WindowId::from(id),
            0,
        )
        .map_err(|err| std::io::Error::other(err.to_string()))?;

        let channel = machine.channel();
        let _io_thread = machine.spawn();

        Ok(Self {
            term: term.clone(),
            size: terminal_size,
            channel,
            last_content: initial_content,
            url_regex: RegexSearch::new(URL_REGEX).expect("invalid url regexp"),
        })
    }

    pub fn handle(&mut self, cmd: Command) -> Action {
        let mut action = Action::default();
        let term = self.term.clone();
        let mut term = term.lock();
        match cmd {
            Command::ProcessRioEvent(event) => {
                match event {
                    RioEvent::Exit
                    | RioEvent::Quit
                    | RioEvent::CloseTerminal(_) => {
                        action = Action::Shutdown;
                    },
                    RioEvent::Title(title)
                    | RioEvent::TitleWithSubtitle(title, _) => {
                        action = Action::ChangeTitle(title);
                    },
                    RioEvent::ResetTitle => {
                        action = Action::ChangeTitle(String::new());
                    },
                    RioEvent::PtyWrite(_, text) => {
                        self.write(text.into_bytes());
                    },
                    _ => {},
                };
            },
            Command::Write(input) => {
                self.write(input);
                term.scroll_display(Scroll::Bottom);
            },
            Command::Scroll(delta) => {
                self.scroll(&mut term, delta);
            },
            Command::Resize(layout_size, font_measure) => {
                self.resize(&mut term, layout_size, font_measure);
            },
            Command::SelectStart(selection_type, (x, y)) => {
                self.start_selection(&mut term, selection_type, x, y);
            },
            Command::SelectUpdate((x, y)) => {
                self.update_selection(&mut term, x, y);
            },
            Command::ProcessLink(link_action, point) => {
                self.process_link_action(&term, link_action, point);
            },
            Command::MouseReport(button, modifiers, point, pressed) => {
                self.process_mouse_report(button, modifiers, point, pressed);
            },
        };

        action
    }

    fn process_link_action(
        &mut self,
        terminal: &Crosswords<EventProxy>,
        link_action: LinkAction,
        point: Pos,
    ) {
        match link_action {
            LinkAction::Hover => {
                self.last_content.hovered_hyperlink = self.regex_match_at(
                    terminal,
                    point,
                    &mut self.url_regex.clone(),
                );
            },
            LinkAction::Clear => {
                self.last_content.hovered_hyperlink = None;
            },
            LinkAction::Open => {
                self.open_link();
            },
        };
    }

    fn open_link(&self) {
        if let Some(range) = &self.last_content.hovered_hyperlink {
            let start = range.start();
            let end = range.end();

            let mut url = String::from(self.last_content.grid[*start].c());
            for indexed in self.last_content.grid.iter_from(*start) {
                url.push(indexed.square.c());
                if indexed.pos == *end {
                    break;
                }
            }

            open::that(url).unwrap_or_else(|_| {
                panic!("link opening is failed");
            })
        }
    }

    fn process_mouse_report(
        &self,
        button: MouseButton,
        modifiers: Modifiers,
        point: Pos,
        pressed: bool,
    ) {
        let mut mods = 0;
        if modifiers.contains(Modifiers::SHIFT) {
            mods += 4;
        }
        if modifiers.contains(Modifiers::ALT) {
            mods += 8;
        }
        if modifiers.contains(Modifiers::COMMAND) {
            mods += 16;
        }

        match MouseMode::from(self.last_content.terminal_mode) {
            MouseMode::Sgr => {
                self.sgr_mouse_report(point, button as u8 + mods, pressed)
            },
            MouseMode::Normal(is_utf8) => {
                if pressed {
                    self.normal_mouse_report(
                        point,
                        button as u8 + mods,
                        is_utf8,
                    )
                } else {
                    self.normal_mouse_report(point, 3 + mods, is_utf8)
                }
            },
        }
    }

    fn sgr_mouse_report(&self, point: Pos, button: u8, pressed: bool) {
        let c = if pressed { 'M' } else { 'm' };

        let msg = format!(
            "\x1b[<{};{};{}{}",
            button,
            point.col + 1,
            point.row + 1,
            c
        );

        self.write(msg.into_bytes());
    }

    fn normal_mouse_report(&self, point: Pos, button: u8, is_utf8: bool) {
        let Pos { row: line, col: column } = point;
        let max_point = if is_utf8 { 2015 } else { 223 };

        if line.0 >= max_point || column.0 >= max_point as usize {
            return;
        }

        let mut msg = vec![b'\x1b', b'[', b'M', 32 + button];

        let mouse_pos_encode = |pos: usize| -> Vec<u8> {
            let pos = 32 + 1 + pos;
            let first = 0xC0 + pos / 64;
            let second = 0x80 + (pos & 63);
            vec![first as u8, second as u8]
        };

        if is_utf8 && column >= Column(95) {
            msg.append(&mut mouse_pos_encode(column.0));
        } else {
            msg.push(32 + 1 + column.0 as u8);
        }

        if is_utf8 && line >= 95 {
            msg.append(&mut mouse_pos_encode(line.0 as usize));
        } else {
            msg.push(32 + 1 + line.0 as u8);
        }

        self.write(msg);
    }

    fn start_selection(
        &mut self,
        terminal: &mut Crosswords<EventProxy>,
        selection_type: SelectionType,
        x: f32,
        y: f32,
    ) {
        let location = Self::selection_point(
            x,
            y,
            &self.size,
            terminal.grid.display_offset(),
        );
        terminal.selection = Some(Selection::new(
            selection_type,
            location,
            self.selection_side(x),
        ));
    }

    fn update_selection(
        &mut self,
        terminal: &mut Crosswords<EventProxy>,
        x: f32,
        y: f32,
    ) {
        let display_offset = terminal.grid.display_offset();
        if let Some(ref mut selection) = terminal.selection {
            let location =
                Self::selection_point(x, y, &self.size, display_offset);
            selection.update(location, self.selection_side(x));
        }
    }

    pub fn selection_point(
        x: f32,
        y: f32,
        terminal_size: &TerminalSize,
        display_offset: usize,
    ) -> Pos {
        let col = (x as usize) / (terminal_size.cell_width as usize);
        let col = min(Column(col), Column(terminal_size.num_cols as usize - 1));

        let line = (y as usize) / (terminal_size.cell_height as usize);
        let line = min(line, terminal_size.num_lines as usize - 1);

        // viewport_to_point: translate a viewport line into a grid line by
        // subtracting the display offset (scrollback amount).
        Pos::new(Line(line as i32) - display_offset, col)
    }

    fn selection_side(&self, x: f32) -> Side {
        let cell_x = x as usize % self.size.cell_width as usize;
        let half_cell_width = (self.size.cell_width as f32 / 2.0) as usize;

        if cell_x > half_cell_width {
            Side::Right
        } else {
            Side::Left
        }
    }

    fn resize(
        &mut self,
        terminal: &mut Crosswords<EventProxy>,
        layout_size: Option<Size<f32>>,
        font_measure: Option<Size<f32>>,
    ) {
        if let Some(size) = layout_size {
            self.size.layout_height = size.height;
            self.size.layout_width = size.width;
        };

        if let Some(size) = font_measure {
            self.size.cell_height = size.height as u16;
            self.size.cell_width = size.width as u16;
        }

        let lines = (self.size.layout_height / self.size.cell_height as f32)
            .floor() as u16;
        let cols = (self.size.layout_width / self.size.cell_width as f32)
            .floor() as u16;
        if lines > 0 && cols > 0 {
            self.size.num_lines = lines;
            self.size.num_cols = cols;
            let _ = self.channel.send(Msg::Resize(WinsizeBuilder {
                rows: self.size.num_lines,
                cols: self.size.num_cols,
                width: self.size.layout_width as u16,
                height: self.size.layout_height as u16,
            }));
            terminal.resize(self.size);
        }
    }

    fn write<I: Into<Cow<'static, [u8]>>>(&self, input: I) {
        let _ = self.channel.send(Msg::Input(input.into()));
    }

    fn scroll(
        &mut self,
        terminal: &mut Crosswords<EventProxy>,
        delta_value: i32,
    ) {
        if delta_value != 0 {
            let scroll = Scroll::Delta(delta_value);
            if terminal
                .mode()
                .contains(TermMode::ALTERNATE_SCROLL | TermMode::ALT_SCREEN)
            {
                let line_cmd = if delta_value > 0 { b'A' } else { b'B' };
                let mut content = vec![];

                for _ in 0..delta_value.abs() {
                    content.push(0x1b);
                    content.push(b'O');
                    content.push(line_cmd);
                }

                self.write(content);
            } else {
                terminal.scroll_display(scroll);
            }
        }
    }

    pub fn selectable_content(&self) -> String {
        let content = self.renderable_content();
        let mut result = String::new();
        if let Some(range) = content.selectable_range {
            for indexed in content.grid.display_iter() {
                if range.contains(indexed.pos) {
                    result.push(indexed.square.c());
                }
            }
        }
        result
    }

    pub fn sync(&mut self) {
        let term = self.term.clone();
        let mut term = term.lock();
        self.internal_sync(&mut term);
    }

    fn internal_sync(&mut self, terminal: &mut Crosswords<EventProxy>) {
        let selectable_range = match &terminal.selection {
            Some(s) => s.to_range(terminal),
            None => None,
        };

        let cursor = *terminal.grid.cursor_cell();
        self.last_content.grid = terminal.grid.clone();
        self.last_content.selectable_range = selectable_range;
        self.last_content.cursor = cursor;
        self.last_content.terminal_mode = terminal.mode();
        self.last_content.terminal_size = self.size;
    }

    pub fn renderable_content(&self) -> &RenderableContent {
        &self.last_content
    }

    /// Based on alacritty/src/display/hint.rs > regex_match_at
    /// Retrieve the match, if the specified point is inside the content matching the regex.
    fn regex_match_at(
        &self,
        terminal: &Crosswords<EventProxy>,
        point: Pos,
        regex: &mut RegexSearch,
    ) -> Option<Match> {
        visible_regex_match_iter(terminal, regex).find(|rm| rm.contains(&point))
    }
}

/// Copied from alacritty/src/display/hint.rs:
/// Iterate over all visible regex matches.
fn visible_regex_match_iter<'a>(
    term: &'a Crosswords<EventProxy>,
    regex: &'a mut RegexSearch,
) -> impl Iterator<Item = Match> + 'a {
    let viewport_start = Line(-(term.grid.display_offset() as i32));
    let viewport_end = viewport_start + term.bottommost_line();
    let mut start = term.line_search_left(Pos::new(viewport_start, Column(0)));
    let mut end = term.line_search_right(Pos::new(viewport_end, Column(0)));
    start.row = start.row.max(viewport_start - 100);
    end.row = end.row.min(viewport_end + 100);

    RegexIter::new(start, end, Direction::Right, term, regex)
        .skip_while(move |rm| rm.end().row < viewport_start)
        .take_while(move |rm| rm.start().row <= viewport_end)
}

pub struct RenderableContent {
    pub grid: Grid<Square>,
    pub hovered_hyperlink: Option<RangeInclusive<Pos>>,
    pub selectable_range: Option<SelectionRange>,
    pub cursor: Square,
    pub terminal_mode: TermMode,
    pub terminal_size: TerminalSize,
}

impl Default for RenderableContent {
    fn default() -> Self {
        Self {
            grid: Grid::new(0, 0, 0),
            hovered_hyperlink: None,
            selectable_range: None,
            cursor: Square::default(),
            terminal_mode: TermMode::empty(),
            terminal_size: TerminalSize::default(),
        }
    }
}

/// Resolve a square's foreground/background colors (as `AnsiColor`) from the
/// per-grid style side-table. rio-vt packs each cell into a `u64`; text cells
/// index into `style_set`, while bg-only cells encode the background inline.
pub(crate) fn square_colors(
    square: Square,
    styles: &[rio_vt::crosswords::style::Style],
) -> (AnsiColor, AnsiColor, rio_vt::crosswords::style::StyleFlags) {
    use rio_vt::crosswords::style::StyleFlags;
    match square.content_tag() {
        ContentTag::Codepoint => {
            let style = styles
                .get(square.style_id() as usize)
                .copied()
                .unwrap_or_default();
            (style.fg, style.bg, style.flags)
        },
        ContentTag::BgPalette => (
            AnsiColor::Named(NamedColor::Foreground),
            AnsiColor::Indexed(square.bg_palette_index()),
            StyleFlags::empty(),
        ),
        ContentTag::BgRgb => {
            let (r, g, b) = square.bg_rgb();
            (
                AnsiColor::Named(NamedColor::Foreground),
                AnsiColor::Spec(ColorRgb { r, g, b }),
                StyleFlags::empty(),
            )
        },
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.channel.send(Msg::Shutdown);
    }
}

#[derive(Clone)]
pub struct EventProxy(mpsc::Sender<RioEvent>);

impl EventListener for EventProxy {
    fn event(&self) -> (Option<RioEvent>, bool) {
        (None, false)
    }

    fn send_event(&self, event: RioEvent, _id: WindowId) {
        let _ = self.0.try_send(event);
    }

    fn send_event_with_high_priority(&self, event: RioEvent, _id: WindowId) {
        let _ = self.0.try_send(event);
    }
}
