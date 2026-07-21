use crate::actions::Action;
use crate::settings::BackendSettings;
use alacritty_terminal::event::{
    Event, EventListener, Notify, OnResize, WindowSize,
};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{
    self,
    cell::{Cell, Flags},
    test::TermSize,
    viewport_to_point, Term, TermMode,
};
use alacritty_terminal::{tty, Grid};
use iced::keyboard::Modifiers;
use iced_core::Size;
use std::borrow::Cow;
use std::cmp::min;
use std::io::Result;
use std::ops::{Index, RangeInclusive};
use std::sync::Arc;
use tokio::sync::mpsc;

const URL_REGEX: &str = r#"(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file://|git://|ssh:|ftp://)[^\u{0000}-\u{001F}\u{007F}-\u{009F}<>"\s{-}\^⟨⟩`]+"#;

#[derive(Debug, Clone)]
pub enum Command {
    Write(Vec<u8>),
    Scroll(i32),
    Resize(Option<Size<f32>>, Option<Size<f32>>),
    SelectStart(SelectionType, (f32, f32)),
    SelectUpdate((f32, f32)),
    ProcessLink(LinkAction, Point),
    MouseReport(MouseButton, Modifiers, Point, bool),
    ProcessAlacrittyEvent(Event),
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

impl From<TerminalSize> for WindowSize {
    fn from(size: TerminalSize) -> Self {
        Self {
            num_lines: size.num_lines,
            num_cols: size.num_cols,
            cell_width: size.cell_width,
            cell_height: size.cell_height,
        }
    }
}

pub struct Backend {
    term: Arc<FairMutex<Term<EventProxy>>>,
    size: TerminalSize,
    notifier: Notifier,
    last_content: RenderableContent,
    pub(crate) url_regex: RegexSearch,
}

impl Backend {
    pub fn new(
        id: u64,
        pty_event_proxy_sender: mpsc::Sender<Event>,
        settings: BackendSettings,
    ) -> Result<Self> {
        let pty_config = tty::Options {
            shell: Some(tty::Shell::new(settings.program, settings.args)),
            working_directory: settings.working_directory,
            env: settings.env,
            ..tty::Options::default()
        };

        let config = term::Config::default();
        let terminal_size = TerminalSize::default();
        let pty = tty::new(&pty_config, terminal_size.into(), id)?;

        let event_proxy = EventProxy(pty_event_proxy_sender);

        let mut term = Term::new(config, &terminal_size, event_proxy.clone());

        let cursor = term.grid_mut().cursor_cell().clone();

        let initial_content = RenderableContent {
            grid: term.grid().clone(),
            selectable_range: None,
            terminal_mode: *term.mode(),
            terminal_size,
            cursor: cursor.clone(),
            hovered_hyperlink: None,
        };

        let term = Arc::new(FairMutex::new(term));

        let pty_event_loop =
            EventLoop::new(term.clone(), event_proxy, pty, false, false)?;

        let notifier = Notifier(pty_event_loop.channel());

        let _ = pty_event_loop.spawn();

        Ok(Self {
            term: term.clone(),
            size: terminal_size,
            notifier,
            last_content: initial_content,
            url_regex: RegexSearch::new(URL_REGEX).expect("invalid url regexp"),
        })
    }

    pub fn handle(&mut self, cmd: Command) -> Action {
        let mut action = Action::default();
        let term = self.term.clone();
        let mut term = term.lock();
        match cmd {
            Command::ProcessAlacrittyEvent(event) => {
                match event {
                    Event::Exit => {
                        action = Action::Shutdown;
                    },
                    Event::Title(title) => {
                        action = Action::ChangeTitle(title);
                    },
                    Event::PtyWrite(pty) => {
                        self.notifier.notify(pty.into_bytes())
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
        terminal: &Term<EventProxy>,
        link_action: LinkAction,
        point: Point,
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

            let mut url = String::from(self.last_content.grid.index(*start).c);
            for indexed in self.last_content.grid.iter_from(*start) {
                url.push(indexed.c);
                if indexed.point == *end {
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
        point: Point,
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

    fn sgr_mouse_report(&self, point: Point, button: u8, pressed: bool) {
        let c = if pressed { 'M' } else { 'm' };

        let msg = format!(
            "\x1b[<{};{};{}{}",
            button,
            point.column + 1,
            point.line + 1,
            c
        );

        self.notifier.notify(msg.as_bytes().to_vec());
    }

    fn normal_mouse_report(&self, point: Point, button: u8, is_utf8: bool) {
        let Point { line, column } = point;
        let max_point = if is_utf8 { 2015 } else { 223 };

        if line >= max_point || column >= max_point {
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

        self.notifier.notify(msg);
    }

    fn start_selection(
        &mut self,
        terminal: &mut Term<EventProxy>,
        selection_type: SelectionType,
        x: f32,
        y: f32,
    ) {
        let location = Self::selection_point(
            x,
            y,
            &self.size,
            terminal.grid().display_offset(),
        );
        terminal.selection = Some(Selection::new(
            selection_type,
            location,
            self.selection_side(x),
        ));
    }

    fn update_selection(
        &mut self,
        terminal: &mut Term<EventProxy>,
        x: f32,
        y: f32,
    ) {
        let display_offset = terminal.grid().display_offset();
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
    ) -> Point {
        let col = (x as usize) / (terminal_size.cell_width as usize);
        let col = min(Column(col), Column(terminal_size.num_cols as usize - 1));

        let line = (y as usize) / (terminal_size.cell_height as usize);
        let line = min(line, terminal_size.num_lines as usize - 1);

        viewport_to_point(display_offset, Point::new(line, col))
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
        terminal: &mut Term<EventProxy>,
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
            self.notifier.on_resize(self.size.into());
            terminal.resize(TermSize::new(
                self.size.num_cols as usize,
                self.size.num_lines as usize,
            ));
        }
    }

    fn write<I: Into<Cow<'static, [u8]>>>(&self, input: I) {
        self.notifier.notify(input);
    }

    fn scroll(&mut self, terminal: &mut Term<EventProxy>, delta_value: i32) {
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

                self.notifier.notify(content);
            } else {
                terminal.grid_mut().scroll_display(scroll);
            }
        }
    }

    pub fn selectable_content(&self) -> String {
        let content = self.renderable_content();
        let Some(range) = content.selectable_range else {
            return String::new();
        };

        // Group the selected cells BY LINE before rebuilding the text. `display_iter` walks the grid
        // row-major (increasing columns, then next line); we accumulate the selected chars of the current
        // line and remember whether its LAST column carries the WRAPLINE flag (soft-wrapped onto the next
        // line). A line can be traversed without any selected cell (before/after the range): `selected`
        // distinguishes it so we do not emit an empty line for it.
        let mut lines: Vec<SelectedLine> = Vec::new();
        let mut cur_line: Option<i32> = None;
        let mut chars: Vec<char> = Vec::new();
        let mut selected = false;
        let mut wrapped = false;
        for indexed in content.grid.display_iter() {
            let line = indexed.point.line.0;
            if cur_line != Some(line) {
                // `chars` is drained by `mem::take` when the line was selected; otherwise it is already
                // empty (we only push under `range.contains`, which also sets `selected`).
                if selected {
                    lines.push(SelectedLine {
                        chars: std::mem::take(&mut chars),
                        wrapped,
                    });
                }
                cur_line = Some(line);
                selected = false;
                // `wrapped` is not reset here: every cell of the new line rewrites it below, so at the
                // next flush it holds the value of the last column.
            }
            if range.contains(indexed.point) {
                chars.push(indexed.c);
                selected = true;
            }
            // The WRAPLINE flag lives on the LAST cell (last column) of the physical line. Since columns
            // are walked in order, the last write before a line change is that column, so its value wins
            // at flush time.
            wrapped = indexed.cell.flags.contains(Flags::WRAPLINE);
        }
        if selected {
            lines.push(SelectedLine { chars, wrapped });
        }

        join_selected_lines(&lines, range.is_block)
    }

    pub fn sync(&mut self) {
        let term = self.term.clone();
        let mut term = term.lock();
        self.internal_sync(&mut term);
    }

    fn internal_sync(&mut self, terminal: &mut Term<EventProxy>) {
        let selectable_range = match &terminal.selection {
            Some(s) => s.to_range(terminal),
            None => None,
        };

        let cursor = terminal.grid_mut().cursor_cell().clone();
        self.last_content.grid = terminal.grid().clone();
        self.last_content.selectable_range = selectable_range;
        self.last_content.cursor = cursor.clone();
        self.last_content.terminal_mode = *terminal.mode();
        self.last_content.terminal_size = self.size;
    }

    pub fn renderable_content(&self) -> &RenderableContent {
        &self.last_content
    }

    /// Based on alacritty/src/display/hint.rs > regex_match_at
    /// Retrieve the match, if the specified point is inside the content matching the regex.
    fn regex_match_at(
        &self,
        terminal: &Term<EventProxy>,
        point: Point,
        regex: &mut RegexSearch,
    ) -> Option<Match> {
        let x = visible_regex_match_iter(terminal, regex)
            .find(|rm| rm.contains(&point));
        x
    }
}

// One line of the rebuilt selection: the selected chars in column order, and whether the physical line
// is soft-wrapped (WRAPLINE) onto the next one.
#[derive(Debug, Clone, PartialEq)]
struct SelectedLine {
    chars: Vec<char>,
    wrapped: bool,
}

// Rebuild the text of a multi-line selection from the collected lines. Rules:
//   - a newline is inserted on a LOGICAL line change, but NOT between a line and its physical wrap (a
//     soft-wrapped logical line stays a single line when copied);
//   - trailing-space padding is trimmed at the end of each logical line, but NOT on a wrapped segment
//     (its trailing spaces can be significant: they precede the continuation of the line);
//   - for a BLOCK (rectangular) selection, wrapping does not apply: each line is independent, so always
//     a newline between them and always the trailing trim.
// Pure function (no grid access): directly testable.
fn join_selected_lines(lines: &[SelectedLine], is_block: bool) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let is_last = i + 1 == lines.len();
        let wrapped = line.wrapped && !is_block;
        if wrapped && !is_last {
            // Wrapped segment: keep the text as is and append the continuation with no separator.
            out.extend(line.chars.iter());
        } else {
            // End of a logical line: trim to the last non-space char (drops the padding), then a newline
            // except for the very last line. `rposition` finds the last non-space; when absent (empty or
            // blank line) it yields an empty slice, which preserves a legitimately empty line.
            let end = line
                .chars
                .iter()
                .rposition(|&c| c != ' ')
                .map_or(0, |p| p + 1);
            out.extend(line.chars[..end].iter());
            if !is_last {
                out.push('\n');
            }
        }
    }
    out
}

/// Copied from alacritty/src/display/hint.rs:
/// Iterate over all visible regex matches.
fn visible_regex_match_iter<'a>(
    term: &'a Term<EventProxy>,
    regex: &'a mut RegexSearch,
) -> impl Iterator<Item = Match> + 'a {
    let viewport_start = Line(-(term.grid().display_offset() as i32));
    let viewport_end = viewport_start + term.bottommost_line();
    let mut start =
        term.line_search_left(Point::new(viewport_start, Column(0)));
    let mut end = term.line_search_right(Point::new(viewport_end, Column(0)));
    start.line = start.line.max(viewport_start - 100);
    end.line = end.line.min(viewport_end + 100);

    RegexIter::new(start, end, Direction::Right, term, regex)
        .skip_while(move |rm| rm.end().line < viewport_start)
        .take_while(move |rm| rm.start().line <= viewport_end)
}

pub struct RenderableContent {
    pub grid: Grid<Cell>,
    pub hovered_hyperlink: Option<RangeInclusive<Point>>,
    pub selectable_range: Option<SelectionRange>,
    pub cursor: Cell,
    pub terminal_mode: TermMode,
    pub terminal_size: TerminalSize,
}

impl Default for RenderableContent {
    fn default() -> Self {
        Self {
            grid: Grid::new(0, 0, 0),
            hovered_hyperlink: None,
            selectable_range: None,
            cursor: Cell::default(),
            terminal_mode: TermMode::empty(),
            terminal_size: TerminalSize::default(),
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

#[derive(Clone)]
pub struct EventProxy(mpsc::Sender<Event>);

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let _ = self.0.try_send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a line to copy (trailing spaces simulate the grid padding up to the right edge).
    fn line(text: &str, wrapped: bool) -> SelectedLine {
        SelectedLine {
            chars: text.chars().collect(),
            wrapped,
        }
    }

    #[test]
    fn join_multiline_inserts_newlines_and_trims_padding() {
        // Three non-wrapped lines with trailing-space padding: we want a newline between each and no
        // trailing spaces.
        let lines = vec![
            line("abc      ", false),
            line("def   ", false),
            line("ghi", false),
        ];
        assert_eq!(join_selected_lines(&lines, false), "abc\ndef\nghi");
    }

    #[test]
    fn join_single_line_is_untouched_except_trailing_padding() {
        // Common case: a single line. No newline added; trailing padding removed; significant inner
        // spaces preserved.
        let lines = vec![line("hello world   ", false)];
        assert_eq!(join_selected_lines(&lines, false), "hello world");
    }

    #[test]
    fn join_wrapped_line_is_not_split() {
        // The first line is the physical wrap of the logical line: no newline, no trim, the continuation
        // is appended directly.
        let lines = vec![line("abcde", true), line("fg", false)];
        assert_eq!(join_selected_lines(&lines, false), "abcdefg");
    }

    #[test]
    fn join_wrapped_line_keeps_significant_trailing_space() {
        // A wrapped segment ending with a real space: the space belongs to the logical line and must not
        // be trimmed (otherwise "foo " + "bar" would become "foobar" instead of "foo bar").
        let lines = vec![line("foo ", true), line("bar", false)];
        assert_eq!(join_selected_lines(&lines, false), "foo bar");
    }

    #[test]
    fn join_block_selection_ignores_wrap() {
        // In block mode the wrapped flag is ignored: each line is independent (newline + trim), even if
        // the cells carry WRAPLINE.
        let lines = vec![line("ab  ", true), line("cd  ", true)];
        assert_eq!(join_selected_lines(&lines, true), "ab\ncd");
    }

    #[test]
    fn join_preserves_blank_middle_line() {
        // A blank line in the middle of the selection stays a blank line (double newline).
        let lines =
            vec![line("foo", false), line("   ", false), line("bar", false)];
        assert_eq!(join_selected_lines(&lines, false), "foo\n\nbar");
    }
}
