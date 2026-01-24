use std::{
    io::{Stdout, stdout},
    process::{Command, Stdio},
    time::Duration,
};

use crossterm::{
    cursor,
    event::{Event as CrosstermEvent, EventStream, KeyEvent},
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{FutureExt, StreamExt};
use ratatui::backend::CrosstermBackend as Backend;
use snafu::prelude::*;
use tokio::{
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
    time::interval,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace, warn};

use crate::{
    actions::{Action, CommandPaletteAction},
    components::Component,
    error::Result,
    events::Event,
    global,
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

    dirty: bool,
    force_full_refresh: bool,
    root: Box<dyn Component>,

    // Config
    frame_rate: f64,
    tick_rate: f64,
    full_refresh_rate: f64,
}

impl App {
    /// Construct a new instance of [`App`].
    pub fn new(root: Box<dyn Component>) -> Result<Self> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (action_tx, action_rx) = mpsc::unbounded_channel();

        global::initialize(event_tx.clone(), action_tx.clone());

        Ok(Self {
            terminal: ratatui::Terminal::new(Backend::new(stdout()))
                .whatever_context("failed to new Terminal")?,
            task: tokio::spawn(async {}),
            cancellation_token: CancellationToken::new(),
            should_quit: false,
            event_rx,
            event_tx,
            action_tx,
            action_rx,
            dirty: true,
            force_full_refresh: false,
            root,
            frame_rate: 60.0,
            tick_rate: 4.0,
            full_refresh_rate: 1.0 / 30.0, // every 30 seconds
        })
    }

    pub fn set_root(&mut self, component: Box<dyn Component>) {
        self.root = component
    }

    pub fn send_action(&self, action: Action) {
        self.action_tx.send(action).unwrap()
    }

    #[inline]
    fn send_event(&self, event: Event) {
        self.event_tx.send(event).unwrap()
    }

    #[inline]
    fn cancel(&self) {
        self.cancellation_token.cancel();
    }

    pub fn start(&mut self) {
        self.cancel(); // Cancel any existing task
        self.cancellation_token = CancellationToken::new();
        let event_loop = Self::event_loop(
            self.event_tx.clone(),
            self.cancellation_token.clone(),
            self.frame_rate,
            self.tick_rate,
            self.full_refresh_rate,
        );
        self.send_event(Event::Init);
        self.task = tokio::spawn(async {
            event_loop.await;
        });
    }

    pub fn enter(&mut self) -> Result<()> {
        crossterm::terminal::enable_raw_mode().whatever_context("failed to enable raw mode")?;
        crossterm::execute!(stdout(), EnterAlternateScreen, cursor::Hide)
            .whatever_context("failed to enter alter screen")?;
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
        if crossterm::terminal::is_raw_mode_enabled()
            .whatever_context("failed to check raw mode enabled")?
        {
            self.terminal
                .flush()
                .whatever_context("failed to flush terminal")?;
            crossterm::execute!(stdout(), LeaveAlternateScreen, cursor::Show)
                .whatever_context("faile to leave alter screen")?;
            crossterm::terminal::disable_raw_mode()
                .whatever_context("failed to disable raw mode")?;
        }
        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
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
        tick_rate: f64,
        full_refresh_rate: f64,
    ) {
        let mut event_stream = EventStream::new();
        let mut tick_interval = interval(Duration::from_secs_f64(1.0 / tick_rate));
        let mut render_interval = interval(Duration::from_secs_f64(1.0 / frame_rate));
        let mut full_refresh_interval = interval(Duration::from_secs_f64(1.0 / full_refresh_rate));

        loop {
            let event = tokio::select! {
                _ = cancellation_token.cancelled() => {
                    break;
                },
                _ = tick_interval.tick() => Event::Tick,
                _ = render_interval.tick() => Event::Render,
                _ = full_refresh_interval.tick() => Event::FullRefresh,
                crossterm_event = event_stream.next().fuse() => match crossterm_event {
                    Some(Ok(event)) => match event {
                        CrosstermEvent::Key(key) => Event::Key(key),
                        CrosstermEvent::Mouse(mouse) => Event::Mouse(mouse),
                        CrosstermEvent::FocusGained => Event::FocusGained,
                        CrosstermEvent::FocusLost => Event::FocusLost,
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
                Event::Render => {
                    if self.dirty {
                        self.send_action(Action::Render);
                    }
                }
                Event::Dirty => self.dirty = true,
                Event::FullRefresh => {
                    trace!("full refresh triggered");
                    // Set flag to force full refresh in render() to clear diff artifacts
                    self.force_full_refresh = true;
                    self.dirty = true;
                }
                _ => {
                    if !matches!(event, Event::Tick) {
                        tracing::trace!(?event, "handling component event");
                    }
                    self.root.handle_event(&event);
                }
            }
        }
        Ok(())
    }

    fn on_key_event(&mut self, key: KeyEvent) {
        self.root.handle_event(&Event::Key(key));
    }

    fn handle_action(&mut self) -> Result<()> {
        while let Ok(action) = self.action_rx.try_recv() {
            if !matches!(action, Action::Render) {
                debug!(?action, "handle action");
            } else {
                trace!(?action, "handle action");
            }
            match action {
                Action::Quit => self.should_quit = true,
                Action::Render => self.render()?,
                Action::CommandPalette(CommandPaletteAction::Shell) => {
                    self.root.handle_action(&action);
                    self.exit()?;
                    if let Err(err) = self.run_shell() {
                        warn!(?err, "failed to run shell");
                    }
                    self.enter()?;
                    self.terminal
                        .clear()
                        .whatever_context("failed to clear terminal after shell")?;
                    self.dirty = true;
                }
                _ => {
                    self.root.handle_action(&action);
                }
            }
        }
        Ok(())
    }

    fn render(&mut self) -> Result<()> {
        self.terminal
            .draw(|frame| {
                // If a full refresh is requested, first render a blank block
                // to clear any ratatui diff artifacts. Then render the actual content.
                // This clears any leftover characters from the diff algorithm.
                if self.force_full_refresh {
                    let blank = ratatui::widgets::Clear;
                    frame.render_widget(blank, frame.area());
                }

                if let Err(err) = self.root.draw(frame, frame.area()) {
                    error!(?err, "terminal draw error");
                }
            })
            .whatever_context("failed to draw terminal")?;
        self.dirty = false;
        self.force_full_refresh = false;
        Ok(())
    }

    fn run_shell(&self) -> Result<()> {
        let envs = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(code_combo::tools::prepare_mcp_envs())
        });
        let envs = match envs {
            Ok(envs) => envs,
            Err(err) => whatever!("failed to prepare mcp envs: {err}"),
        };
        let mut out = stdout();
        let _ = crossterm::execute!(out, Clear(ClearType::All), cursor::MoveTo(0, 0));
        println!("Entering shell. Type 'exit' to return.");
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let mut cmd = Command::new(&shell);
        for key in code_combo::env_keys_with_prefix("COCO_") {
            cmd.env_remove(key);
        }
        let status = cmd
            .envs(envs)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .whatever_context("failed to spawn shell")?;
        if !status.success() {
            warn!(?status, "shell exited with non-zero status");
        }
        Ok(())
    }
}
