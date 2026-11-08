use crate::parse::{Command, parse_line};
use crate::stock::{Lot, Stock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    Parse {
        line: usize,
    },
    Insufficient {
        line: usize,
        sku: String,
        location: String,
        available: u64,
    },
    NothingToUndo {
        line: usize,
    },
}

#[derive(Debug, Clone, Default)]
pub(crate) struct State {
    pub stock: Stock,
    pub shipped_cost: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Ledger {
    state: State,
    history: Vec<State>,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies the lines of `script` in order.
    pub fn apply(&mut self, script: &str) -> Result<(), LedgerError> {
        for (index, text) in script.lines().enumerate() {
            let line = index + 1;
            let Some(command) = parse_line(text).map_err(|()| LedgerError::Parse { line })? else {
                continue;
            };
            if command == Command::Undo {
                self.state = self
                    .history
                    .pop()
                    .ok_or(LedgerError::NothingToUndo { line })?;
                continue;
            }
            let before = self.state.clone();
            execute(&mut self.state, command, line)?;
            self.history.push(before);
        }
        Ok(())
    }

    pub fn quantity(&self, sku: &str, location: &str) -> u64 {
        self.state.stock.quantity(sku, location)
    }

    pub fn value_cents(&self, sku: &str, location: &str) -> u64 {
        self.state.stock.value(sku, location)
    }

    pub fn shipped_cost_cents(&self) -> u64 {
        self.state.shipped_cost
    }

    pub fn report(&self) -> String {
        crate::report::render(&self.state)
    }
}

fn execute(state: &mut State, command: Command, line: usize) -> Result<(), LedgerError> {
    match command {
        Command::Receive {
            sku,
            quantity,
            location,
            unit_cost,
        } => state.stock.receive(
            &sku,
            &location,
            Lot {
                quantity,
                unit_cost,
            },
        ),
        Command::Ship {
            sku,
            quantity,
            location,
        } => {
            let lots = state
                .stock
                .take(&sku, &location, quantity)
                .map_err(|available| insufficient(line, &sku, &location, available))?;
            state.shipped_cost += lots.iter().map(Lot::value).sum::<u64>();
        }
        Command::Move {
            sku,
            quantity,
            from,
            to,
        } => {
            let lots = state
                .stock
                .take(&sku, &from, quantity)
                .map_err(|available| insufficient(line, &sku, &from, available))?;
            for lot in lots {
                state.stock.receive(&sku, &to, lot);
            }
        }
        Command::Undo => {}
    }
    Ok(())
}

fn insufficient(line: usize, sku: &str, location: &str, available: u64) -> LedgerError {
    LedgerError::Insufficient {
        line,
        sku: sku.to_owned(),
        location: location.to_owned(),
        available,
    }
}
