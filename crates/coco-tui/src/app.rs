use std::{
    io::{Stdout, stdout},
    time::Duration,
};

use color_eyre::Result;
use crossterm::{
    cursor,
    event::{Event as CrosstermEvent, EventStream, KeyCode, KeyEvent, KeyModifiers},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{FutureExt, StreamExt};
use ratatui::backend::CrosstermBackend as Backend;
use tokio::{
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
    time::interval,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::{
    actions::Action,
    components::{Chat, Component},
    events::Event,
};

pub struct App {
    terminal: ratatui::Terminal<Backend<Stdout>>,
    task: JoinHandle<()>,
    cancellation_token: CancellationToken,
    should_quit: bool,

    event_rx: UnboundedReceiver<Event>,
    event_tx: UnboundedSender<Event>,
    action_rx: UnboundedReceiver<Action>,
    action_tx: UnboundedSender<Action>,

    root: Box<dyn Component>,

    // Config
    frame_rate: f64,
}

impl App {
    /// Construct a new instance of [`App`].
    pub fn new() -> Result<Self> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (action_tx, action_rx) = mpsc::unbounded_channel();
        Ok(Self {
            terminal: ratatui::Terminal::new(Backend::new(stdout()))?,
            task: tokio::spawn(async {}),
            cancellation_token: CancellationToken::new(),
            should_quit: false,
            event_rx,
            event_tx,
            action_tx,
            action_rx,
            frame_rate: 60.0,
            root: Box::new(Chat::default()),
        })
    }

    #[inline]
    fn send_action(&self, action: Action) {
        self.action_tx.send(action).unwrap()
    }

    pub fn cancel(&self) {
        self.cancellation_token.cancel();
    }

    pub fn start(&mut self) {
        self.cancel(); // Cancel any existing task
        self.cancellation_token = CancellationToken::new();
        let event_loop = Self::event_loop(
            self.event_tx.clone(),
            self.cancellation_token.clone(),
            self.frame_rate,
        );
        self.task = tokio::spawn(async {
            event_loop.await;
        });
    }

    pub fn enter(&mut self) -> Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(stdout(), EnterAlternateScreen, cursor::Hide)?;
        self.start();
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.cancel();
        let mut counter = 0;
        while !self.task.is_finished() {
            std::thread::sleep(Duration::from_millis(1));
            counter += 1;
            if counter > 50 {
                self.task.abort();
            }
            if counter > 100 {
                error!("Failed to abort task in 100 milliseconds for unknown reason");
                break;
            }
        }
        Ok(())
    }

    pub fn exit(&mut self) -> Result<()> {
        self.stop()?;
        if crossterm::terminal::is_raw_mode_enabled()? {
            self.terminal.flush()?;
            crossterm::execute!(stdout(), LeaveAlternateScreen, cursor::Show)?;
            crossterm::terminal::disable_raw_mode()?;
        }
        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        self.root
            .config(self.action_tx.clone(), self.event_tx.clone());

        self.enter()?;
        loop {
            self.handle_event().await?;
            self.handle_action()?;
            if self.should_quit {
                break;
            }
        }
        self.exit()?;
        Ok(())
    }

    async fn event_loop(
        event_tx: UnboundedSender<Event>,
        cancellation_token: CancellationToken,
        frame_rate: f64,
    ) {
        let mut event_stream = EventStream::new();
        let mut render_interval = interval(Duration::from_secs_f64(1.0 / frame_rate));

        // if this fails, then it's likely a bug in the calling code
        event_tx
            .send(Event::Init)
            .expect("failed to send init event");
        loop {
            let event = tokio::select! {
                _ = cancellation_token.cancelled() => {
                    break;
                }
                _ = render_interval.tick() => Event::Render,
                crossterm_event = event_stream.next().fuse() => match crossterm_event {
                    Some(Ok(event)) => match event {
                        CrosstermEvent::Key(key) => Event::Key(key),
                        CrosstermEvent::Mouse(mouse) => Event::Mouse(mouse),
                        _ => continue, // ignore other events
                    }
                    Some(Err(err)) => {
                        warn!(?err, "Receive next crossterm event error, ignoring...");
                        continue
                    },
                    None => break, // the event stream has stopped and will not produce any more events
                },
            };
            if event_tx.send(event).is_err() {
                // the receiver has been dropped, so there's no point in continuing the loop
                break;
            }
        }
        cancellation_token.cancel();
    }

    async fn handle_event(&mut self) -> Result<()> {
        if let Some(event) = self.event_rx.recv().await {
            match event {
                Event::Key(key) => self.on_key_event(key),
                Event::Render => self.send_action(Action::Render),
                _ => {
                    self.root.handle_event(&event);
                }
            }
        }
        Ok(())
    }

    fn on_key_event(&mut self, key: KeyEvent) {
        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Char('c') | KeyCode::Char('C')) => {
                self.send_action(Action::Quit);
            }
            _ => {
                self.root.handle_event(&Event::Key(key));
            }
        }
    }

    fn handle_action(&mut self) -> Result<()> {
        while let Ok(action) = self.action_rx.try_recv() {
            if action != Action::Render {
                debug!(?action, "handle action");
            }
            match action {
                Action::Quit => self.should_quit = true,
                Action::Render => self.render()?,
                _ => {
                    self.root.handle_action(&action);
                }
            }
        }
        Ok(())
    }
    fn render(&mut self) -> Result<()> {
        self.terminal.draw(|frame| {
            if let Err(err) = self.root.draw(frame, frame.area()) {
                error!(?err, "terminal draw error");
            }
        })?;
        Ok(())
    }
}
