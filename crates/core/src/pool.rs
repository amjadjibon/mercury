//! Memory pool for zero-allocation operations in hot paths.

use parking_lot::Mutex;
use std::mem::MaybeUninit;

/// Pre-allocated memory pool for reusable objects.
///
/// This pool allows objects to be borrowed and returned without
/// heap allocations, which is critical for low-latency trading.
pub struct Pool<T> {
    items: Mutex<Vec<T>>,
    capacity: usize,
}

impl<T> Pool<T> {
    /// Create a new pool with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            items: Mutex::new(Vec::with_capacity(capacity)),
            capacity,
        }
    }

    /// Create a new pool pre-filled with items.
    pub fn with_items<F>(capacity: usize, factory: F) -> Self
    where
        F: Fn() -> T,
    {
        let items: Vec<T> = (0..capacity).map(|_| factory()).collect();
        Self {
            items: Mutex::new(items),
            capacity,
        }
    }

    /// Get an item from the pool, or create a new one if empty.
    pub fn get<F>(&self, factory: F) -> T
    where
        F: FnOnce() -> T,
    {
        self.items.lock().pop().unwrap_or_else(factory)
    }

    /// Return an item to the pool.
    ///
    /// If the pool is at capacity, the item is dropped.
    pub fn put(&self, item: T) {
        let mut items = self.items.lock();
        if items.len() < self.capacity {
            items.push(item);
        }
    }

    /// Get the number of available items in the pool.
    pub fn available(&self) -> usize {
        self.items.lock().len()
    }

    /// Get the capacity of the pool.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Check if the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.items.lock().is_empty()
    }
}

impl<T: Default> Pool<T> {
    /// Get an item using Default::default as the factory.
    pub fn get_default(&self) -> T {
        self.get(T::default)
    }
}

impl<T: Clone> Pool<T> {
    /// Create a pool filled with clones of a template.
    pub fn filled(capacity: usize, template: T) -> Self {
        let items: Vec<T> = (0..capacity).map(|_| template.clone()).collect();
        Self {
            items: Mutex::new(items),
            capacity,
        }
    }
}

/// A fixed-size ring buffer for zero-allocation iteration.
pub struct RingBuffer<T, const N: usize> {
    buffer: [MaybeUninit<T>; N],
    head: usize,
    tail: usize,
    len: usize,
}

impl<T, const N: usize> RingBuffer<T, N> {
    /// Create a new empty ring buffer.
    pub fn new() -> Self {
        Self {
            buffer: unsafe { MaybeUninit::uninit().assume_init() },
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    /// Push an item to the buffer, overwriting the oldest if full.
    pub fn push(&mut self, item: T) -> Option<T> {
        let old = if self.len == N {
            // Buffer is full, drop oldest
            let old = unsafe { self.buffer[self.head].assume_init_read() };
            self.head = (self.head + 1) % N;
            Some(old)
        } else {
            self.len += 1;
            None
        };

        self.buffer[self.tail].write(item);
        self.tail = (self.tail + 1) % N;
        old
    }

    /// Pop the oldest item from the buffer.
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }

        let item = unsafe { self.buffer[self.head].assume_init_read() };
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(item)
    }

    /// Get the number of items in the buffer.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Check if the buffer is full.
    pub fn is_full(&self) -> bool {
        self.len == N
    }

    /// Get the capacity of the buffer.
    pub const fn capacity(&self) -> usize {
        N
    }
}

impl<T, const N: usize> Default for RingBuffer<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Drop for RingBuffer<T, N> {
    fn drop(&mut self) {
        // Drop all remaining items
        while self.pop().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_get_put() {
        let pool: Pool<Vec<u8>> = Pool::new(10);

        let mut vec = pool.get(|| Vec::with_capacity(1024));
        vec.push(1);
        vec.push(2);
        vec.clear();
        pool.put(vec);

        let vec2 = pool.get(|| Vec::with_capacity(1024));
        assert_eq!(vec2.capacity(), 1024);
    }

    #[test]
    fn test_pool_capacity() {
        let pool: Pool<i32> = Pool::with_items(5, || 0);
        assert_eq!(pool.available(), 5);

        let _item = pool.get(|| 0);
        assert_eq!(pool.available(), 4);
    }

    #[test]
    fn test_ring_buffer() {
        let mut buf: RingBuffer<i32, 3> = RingBuffer::new();

        buf.push(1);
        buf.push(2);
        buf.push(3);
        assert!(buf.is_full());

        let old = buf.push(4);
        assert_eq!(old, Some(1));

        assert_eq!(buf.pop(), Some(2));
        assert_eq!(buf.pop(), Some(3));
        assert_eq!(buf.pop(), Some(4));
        assert!(buf.is_empty());
    }
}
