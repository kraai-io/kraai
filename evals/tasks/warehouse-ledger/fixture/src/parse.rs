#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    Receive {
        sku: String,
        quantity: u64,
        location: String,
        unit_cost: u64,
    },
    Ship {
        sku: String,
        quantity: u64,
        location: String,
    },
    Move {
        sku: String,
        quantity: u64,
        from: String,
        to: String,
    },
    Undo,
}

/// Parses one script line. Blank lines and comments parse to `None`.
pub(crate) fn parse_line(line: &str) -> Result<Option<Command>, ()> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let words: Vec<&str> = line.split(' ').collect();
    let command = match words.as_slice() {
        ["receive", sku, quantity, location, cost] => Command::Receive {
            sku: (*sku).to_owned(),
            quantity: parse_quantity(quantity)?,
            location: (*location).to_owned(),
            unit_cost: parse_cost(cost)?,
        },
        ["ship", sku, quantity, location] => Command::Ship {
            sku: (*sku).to_owned(),
            quantity: parse_quantity(quantity)?,
            location: (*location).to_owned(),
        },
        ["move", sku, quantity, from, to] => Command::Move {
            sku: (*sku).to_owned(),
            quantity: parse_quantity(quantity)?,
            from: (*from).to_owned(),
            to: (*to).to_owned(),
        },
        ["undo"] => Command::Undo,
        _ => return Err(()),
    };
    Ok(Some(command))
}

fn parse_quantity(text: &str) -> Result<u64, ()> {
    text.parse().map_err(|_| ())
}

fn parse_cost(text: &str) -> Result<u64, ()> {
    text.trim_start_matches('@').parse().map_err(|_| ())
}
