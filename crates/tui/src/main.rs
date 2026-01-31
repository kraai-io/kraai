use color_eyre::eyre::Result;

use crate::app::App;

mod app;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let mut app = App::new().await?;

    let terminal = ratatui::init();

    let result = app.run(terminal).await;

    ratatui::restore();

    result
}
