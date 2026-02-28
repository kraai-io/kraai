use agent_runtime::{Event, RuntimeBuilder};
use color_eyre::eyre::Result;
use crossbeam_channel::{Sender, bounded};

use crate::app::App;

mod app;
mod components;

fn main() -> Result<()> {
    color_eyre::install()?;

    let (event_tx, event_rx): (Sender<Event>, _) = bounded(100);

    let callback = move |event: Event| {
        let _ = event_tx.send(event);
    };

    let runtime = RuntimeBuilder::new(callback).build();

    let mut app = App::new(runtime, event_rx);

    let terminal = ratatui::init();

    let result = app.run(terminal);

    ratatui::restore();

    result
}
