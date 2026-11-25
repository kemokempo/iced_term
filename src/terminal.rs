use crate::actions::Action;
use crate::backend;
use crate::bindings::{Binding, BindingAction, BindingsLayout, InputKind};
use crate::font::TermFont;
use crate::settings::{FontSettings, Settings, ThemeSettings};
use crate::theme::{ColorPalette, Theme};
use crate::AlacrittyEvent;
use iced::futures::stream::BoxStream;
use iced::futures::{SinkExt, StreamExt};
use iced::widget::canvas::Cache;
use iced::Subscription;
use std::hash::{Hash, Hasher};
use std::io::Result;
use std::sync::Arc;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub enum Event {
    BackendCall(u64, backend::Command),
}

#[derive(Debug, Clone)]
pub enum Command {
    ChangeTheme(Box<ColorPalette>),
    ChangeFont(FontSettings),
    AddBindings(Vec<(Binding<InputKind>, BindingAction)>),
    ProxyToBackend(backend::Command),
}

pub struct Terminal {
    pub id: u64,
    widget_id: iced::widget::Id,
    pub(crate) font: TermFont,
    pub(crate) theme: Theme,
    pub(crate) cache: Cache,
    pub(crate) bindings: BindingsLayout,
    pub(crate) backend: backend::Backend,
    backend_event_rx: Arc<Mutex<UnboundedReceiver<AlacrittyEvent>>>,
}

impl Terminal {
    pub fn new(id: u64, settings: Settings) -> Result<Self> {
        let (backend_event_tx, backend_event_rx) = mpsc::unbounded_channel();
        let theme = Theme::new(settings.theme);
        let font = TermFont::new(settings.font);

        Ok(Self {
            id,
            widget_id: iced::widget::Id::unique(),
            font,
            theme,
            bindings: BindingsLayout::default(),
            cache: Cache::default(),
            backend: backend::Backend::new(
                id,
                backend_event_tx,
                settings.backend,
            )?,
            backend_event_rx: Arc::new(Mutex::new(backend_event_rx)),
        })
    }

    pub fn widget_id(&self) -> &iced::widget::Id {
        &self.widget_id
    }

    pub fn subscription(&self) -> Subscription<Event> {
        let data = TerminalSubscriptionData {
            id: self.id,
            event_receiver: self.backend_event_rx.clone(),
        };

        Subscription::run_with(data, terminal_subscription_stream)
    }

    /// The terminal's text, scrollback first and the visible screen last, one
    /// `String` per line.
    ///
    /// For callers that need to read what the terminal shows rather than only
    /// draw it: scraping the output of a command an application wrote into the
    /// PTY itself, asserting on a terminal's contents in a test, or handing the
    /// screen to an accessibility layer. [`Self::handle`] can write into the
    /// PTY already; this is the other half of that conversation.
    ///
    /// A line is a *logical* line, not a grid row: output that wrapped across
    /// the terminal's width comes back joined, so a reader is not looking at
    /// where the window happened to be split. Trailing blanks are trimmed.
    pub fn text(&self) -> Vec<String> {
        self.backend.text()
    }

    pub fn handle(&mut self, cmd: Command) -> Action {
        let mut action = Action::default();

        match cmd {
            Command::ChangeTheme(color_pallete) => {
                self.theme = Theme::new(ThemeSettings::new(color_pallete));
            },
            Command::ChangeFont(font_settings) => {
                self.font = TermFont::new(font_settings);
            },
            Command::AddBindings(bindings) => {
                self.bindings.add_bindings(bindings);
            },
            Command::ProxyToBackend(cmd) => {
                action = self.backend.handle(cmd);
            },
        };

        self.sync_and_redraw();
        action
    }

    fn sync_and_redraw(&mut self) {
        self.sync_font();
        self.backend.sync();
        self.redraw();
    }

    fn sync_font(&mut self) {
        self.font.sync();
        self.backend
            .handle(backend::Command::Resize(None, Some(self.font.measure)));
    }

    fn redraw(&mut self) {
        self.cache.clear();
    }
}

#[derive(Clone)]
struct TerminalSubscriptionData {
    id: u64,
    event_receiver: Arc<Mutex<UnboundedReceiver<AlacrittyEvent>>>,
}

impl Hash for TerminalSubscriptionData {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

fn terminal_subscription_stream(
    data: &TerminalSubscriptionData,
) -> BoxStream<'static, Event> {
    let id = data.id;
    let event_receiver = data.event_receiver.clone();
    iced::stream::channel(1000, async move |mut output| {
        let mut shutdown = false;
        loop {
            let mut event_receiver = event_receiver.lock().await;
            match event_receiver.recv().await {
                Some(event) => {
                    if let AlacrittyEvent::Exit = event {
                        shutdown = true
                    };

                    output
                        .send(Event::BackendCall(id, backend::Command::ProcessAlacrittyEvent(event)))
                        .await
                        .unwrap_or_else(|_| {
                            panic!("iced_term stream {}: sending BackendEventReceived event is failed", id)
                        });
                },
                None => {
                    if !shutdown {
                        panic!("iced_term stream {}: terminal event channel closed unexpected", id);
                    }
                    // Channel closed after an `Exit`: every sender is gone, so
                    // `recv` returns `None` at once and never registers a
                    // wakeup. Returning ENDS the stream, since the closure
                    // drops `output` and that closes the sink. Looping instead
                    // spins on an await point that never pends and holds the
                    // async executor that drives EVERY terminal's
                    // subscription: the others stop being polled and the UI
                    // only redraws on input.
                    return;
                },
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A terminal that ends on its own sends `Exit` THROUGH this stream, then
    // its senders drop and `recv` returns `None` without ever pending. The
    // stream has to END: looping instead holds the async executor that drives
    // every terminal's subscription. The work runs on its own thread, so a
    // loop that spins fails this test on the timeout instead of hanging the
    // whole test binary.
    #[test]
    fn stream_ends_when_channel_closes_after_exit() {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (tx, rx) = mpsc::unbounded_channel();
            tx.send(AlacrittyEvent::Exit).expect("send Exit");
            drop(tx); // Last sender gone: `recv` gives `None` once drained.

            let data = TerminalSubscriptionData {
                id: 1,
                event_receiver: Arc::new(Mutex::new(rx)),
            };
            let mut stream = terminal_subscription_stream(&data);
            let delivered = iced::futures::executor::block_on(async {
                let mut count = 0;
                while stream.next().await.is_some() {
                    count += 1;
                }
                count
            });
            let _ = done_tx.send(delivered);
        });

        let delivered = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("stream never ended: the loop spins on `None`");
        assert_eq!(delivered, 1, "only `Exit` should arrive before the end");
    }
}
