#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub name: String,
    pub qty: u32,
}

#[derive(Debug, Default)]
pub struct Inventory {
    items: Vec<Item>,
}

impl Inventory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `qty` of `name`, merging with an existing entry.
    pub fn add(&mut self, name: &str, qty: u32) {
        match self.find(name) {
            Some(item) => item.qty += qty,
            None => self.items.push(Item { name: name.to_string(), qty }),
        }
    }

    /// Takes `qty` of `name` and returns how many are left.
    pub fn take(&mut self, name: &str, qty: u32) -> Result<u32, String> {
        let item = self.find(name).ok_or(format!("no {name} in stock"))?;
        if item.qty < qty {
            return Err(format!("only {} {name} left", item.qty));
        }
        item.qty -= qty;
        Ok(item.qty)
    }

    /// Drops every entry whose quantity is zero.
    pub fn remove_empty(&mut self) {
        for (i, item) in self.items.iter().enumerate() {
            if item.qty == 0 {
                self.items.remove(i);
            }
        }
    }

    /// The name of the entry with the largest quantity.
    pub fn most_stocked(&self) -> Option<&str> {
        let best = self.items.iter().max_by_key(|item| item.qty)?;
        let name = best.name.clone();
        Some(&name)
    }

    pub fn quantity(&self, name: &str) -> u32 {
        self.find(name).map_or(0, |item| item.qty)
    }

    fn find(&self, name: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.name == name)
    }
}
