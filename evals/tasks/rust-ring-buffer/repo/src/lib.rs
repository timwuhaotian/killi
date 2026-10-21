/// A fixed-capacity ring buffer. Pushing into a full buffer overwrites the oldest item.
pub struct RingBuffer<T> {
    items: Vec<Option<T>>,
    /// Index of the oldest item.
    head: usize,
    len: usize,
}

impl<T: Clone> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be positive");
        Self { items: vec![None; capacity], head: 0, len: 0 }
    }

    pub fn capacity(&self) -> usize {
        self.items.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Adds an item, overwriting the oldest one when the buffer is full.
    pub fn push(&mut self, item: T) {
        let tail = (self.head + self.len) % self.capacity();
        self.items[tail] = Some(item);
        if self.len == self.capacity() {
            self.head += 1;
        } else {
            self.len += 1;
        }
    }

    /// Removes and returns the oldest item.
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let item = self.items[self.head].take();
        self.head = (self.head + 1) % self.capacity();
        self.len -= 1;
        item
    }

    /// The items from oldest to newest.
    pub fn to_vec(&self) -> Vec<T> {
        (0..self.len).filter_map(|i| self.items[(self.head + i) % self.capacity()].clone()).collect()
    }
}
