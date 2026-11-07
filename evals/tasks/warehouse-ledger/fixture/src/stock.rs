use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Lot {
    pub quantity: u64,
    pub unit_cost: u64,
}

impl Lot {
    pub fn value(&self) -> u64 {
        self.quantity * self.unit_cost
    }
}

/// Lots of each SKU at each location, in the order they arrived.
#[derive(Debug, Clone, Default)]
pub(crate) struct Stock {
    lots: BTreeMap<(String, String), VecDeque<Lot>>,
}

impl Stock {
    pub fn receive(&mut self, sku: &str, location: &str, lot: Lot) {
        self.lots
            .entry((sku.to_owned(), location.to_owned()))
            .or_default()
            .push_back(lot);
    }

    pub fn quantity(&self, sku: &str, location: &str) -> u64 {
        self.lots_at(sku, location).map(|lot| lot.quantity).sum()
    }

    pub fn value(&self, sku: &str, location: &str) -> u64 {
        self.lots_at(sku, location).map(Lot::value).sum()
    }

    /// Removes `quantity` units and returns the lots it removed.
    /// Fails with the available quantity when there is not enough.
    pub fn take(&mut self, sku: &str, location: &str, quantity: u64) -> Result<Vec<Lot>, u64> {
        let available = self.quantity(sku, location);
        if available < quantity {
            return Err(available);
        }
        let key = (sku.to_owned(), location.to_owned());
        let Some(lots) = self.lots.get_mut(&key) else {
            return Ok(Vec::new());
        };
        let mut taken = Vec::new();
        let mut remaining = quantity;
        while remaining > 0 {
            let Some(oldest) = lots.back_mut() else {
                break;
            };
            let used = oldest.quantity.min(remaining);
            taken.push(Lot {
                quantity: used,
                unit_cost: oldest.unit_cost,
            });
            oldest.quantity -= used;
            remaining -= used;
            if oldest.quantity == 0 {
                lots.pop_back();
            }
        }
        Ok(taken)
    }

    /// SKU, location, quantity, and value of every stocked pair, sorted by SKU then location.
    pub fn rows(&self) -> impl Iterator<Item = (&str, &str, u64, u64)> {
        self.lots.iter().map(|((sku, location), lots)| {
            (
                sku.as_str(),
                location.as_str(),
                lots.iter().map(|lot| lot.quantity).sum(),
                lots.iter().map(Lot::value).sum(),
            )
        })
    }

    fn lots_at(&self, sku: &str, location: &str) -> impl Iterator<Item = &Lot> {
        self.lots
            .get(&(sku.to_owned(), location.to_owned()))
            .into_iter()
            .flatten()
    }
}
