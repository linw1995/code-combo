mod actions;
mod app;
#[macro_use]
mod components;
mod events;
mod logging;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    crate::logging::init()?;

    let mut app = crate::app::App::new()?;
    let result = app.run().await;
    ratatui::restore();

    result
}
