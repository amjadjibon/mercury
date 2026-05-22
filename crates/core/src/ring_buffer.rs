//! Lock-free MPMC ring buffer for event fan-out.
//!
//! # Design
//! - Pre-allocated `Box<[Slot<T>]>` with power-of-2 capacity (no heap on the hot path).
//! - Publisher claims a sequence with `fetch_add`, writes the value, then commits
//!   the slot by storing `seq + 1` into `slot.sequence`.
//! - Each `Subscriber` tracks its own cursor independently — no contention between readers.
//! - Lossy: if a subscriber falls behind by more than `capacity` events, it skips forward
//!   and reports a `RecvError::Lagged` count.
//!
//! # Safety
//! Slots use `UnsafeCell<MaybeUninit<T>>` for interior mutability. The sequence protocol
//! ensures a subscriber only reads a slot after the publisher has committed it (`Release`
//! store → `Acquire` load). Overwrite-while-reading is avoided in practice by choosing a
//! capacity large enough that no subscriber can fall a full rotation behind (65 536 slots
//! at 10 000 events/sec ≈ 6 s of headroom).

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::Notify;

struct Slot<T> {
    /// Committed sequence: set to `publisher_seq + 1` after the value is written.
    /// Initialized to the slot index so slot `i` is ready for sequence `i`.
    sequence: AtomicU64,
    data: UnsafeCell<MaybeUninit<T>>,
}

unsafe impl<T: Send> Send for Slot<T> {}
unsafe impl<T: Send> Sync for Slot<T> {}

pub(crate) struct RingBufferInner<T> {
    slots: Box<[Slot<T>]>,
    pub(crate) mask: usize,
    pub(crate) capacity: usize,
    pub(crate) publisher_seq: AtomicU64,
    pub(crate) notify: Notify,
    pub(crate) closed: AtomicBool,
}

unsafe impl<T: Send> Send for RingBufferInner<T> {}
unsafe impl<T: Send> Sync for RingBufferInner<T> {}

impl<T> Drop for RingBufferInner<T> {
    fn drop(&mut self) {
        for (i, slot) in self.slots.iter().enumerate() {
            if slot.sequence.load(Ordering::Acquire) > i as u64 {
                unsafe {
                    let _ = (*slot.data.get()).assume_init_read();
                }
            }
        }
    }
}

/// Pre-allocated ring buffer shared between one or more publishers and any
/// number of independent `Subscriber` handles.
pub struct RingBuffer<T: Clone + Send> {
    pub(crate) inner: Arc<RingBufferInner<T>>,
}

impl<T: Clone + Send> Clone for RingBuffer<T> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner) }
    }
}

/// Error returned by non-blocking subscriber operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvError {
    /// No new events since the subscriber's cursor.
    Empty,
    /// Subscriber fell behind; `u64` is the number of events skipped.
    Lagged(u64),
    /// The ring buffer has been closed and no further events will arrive.
    Closed,
}

/// Independent cursor into a `RingBuffer`. Each subscriber tracks its own
/// position — events are never consumed, only read (fan-out semantics).
pub struct Subscriber<T: Clone + Send> {
    inner: Arc<RingBufferInner<T>>,
    cursor: u64,
}

impl<T: Clone + Send> Clone for Subscriber<T> {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner), cursor: self.cursor }
    }
}

impl<T: Clone + Send> RingBuffer<T> {
    /// Allocate a new ring buffer. `capacity` is rounded up to the next power of two.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.next_power_of_two();
        let slots: Box<[Slot<T>]> = (0..capacity)
            .map(|i| Slot {
                sequence: AtomicU64::new(i as u64),
                data: UnsafeCell::new(MaybeUninit::uninit()),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            inner: Arc::new(RingBufferInner {
                slots,
                mask: capacity - 1,
                capacity,
                publisher_seq: AtomicU64::new(0),
                notify: Notify::new(),
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// Create a new subscriber whose cursor starts at the current head.
    /// Events published before this call are not visible to the new subscriber.
    pub fn subscribe(&self) -> Subscriber<T> {
        let cursor = self.inner.publisher_seq.load(Ordering::Acquire);
        Subscriber { inner: Arc::clone(&self.inner), cursor }
    }

    /// Publish a value. Claims a sequence atomically, writes, then commits.
    /// Non-blocking; returns the sequence number assigned.
    pub fn publish(&self, value: T) -> u64 {
        let seq = self.inner.publisher_seq.fetch_add(1, Ordering::Relaxed);
        let slot = &self.inner.slots[seq as usize & self.inner.mask];
        
        // Transitional sequence store to signal to lagging subscribers that the slot is under construction
        slot.sequence.store(seq, Ordering::Release);
        
        // Drop the old value that is currently in the slot if it was initialized in a previous rotation
        if seq >= self.inner.capacity as u64 {
            unsafe {
                let old_val = (*slot.data.get()).assume_init_read();
                std::mem::drop(old_val);
            }
        }
        
        unsafe { (*slot.data.get()).write(value) };
        slot.sequence.store(seq + 1, Ordering::Release);
        self.inner.notify.notify_waiters();
        seq
    }

    /// Signal that no further events will be published. Async subscribers waiting
    /// on `recv_async` will return `RecvError::Closed`.
    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }
}

impl<T: Clone + Send> Subscriber<T> {
    /// Non-blocking poll.
    ///
    /// Returns `Ok(T)` if the next event is ready, `Err(Empty)` if not yet
    /// published, `Err(Lagged(n))` if the subscriber has been lapped (cursor
    /// advanced automatically), or `Err(Closed)` if the buffer is shut down.
    pub fn try_recv(&mut self) -> Result<T, RecvError> {
        let next = self.cursor;
        let slot = &self.inner.slots[next as usize & self.inner.mask];
        let seq = slot.sequence.load(Ordering::Acquire);

        if seq == next + 1 {
            // Safety: publisher committed this slot (seq == next+1) before we read.
            // The Release/Acquire pair ensures the write is visible here.
            let value = unsafe { (*slot.data.get()).assume_init_ref().clone() };
            self.cursor = next + 1;
            Ok(value)
        } else if seq > next + 1 {
            // Publisher has wrapped around and overwritten this slot.
            // Jump cursor forward to just before the latest committed sequence.
            let lapped_to = seq - 1;
            let skipped = lapped_to - next;
            self.cursor = lapped_to;
            Err(RecvError::Lagged(skipped))
        } else if self.inner.closed.load(Ordering::Acquire) {
            Err(RecvError::Closed)
        } else {
            Err(RecvError::Empty)
        }
    }

    /// Blocking spin-wait receive. Suitable for a dedicated OS thread.
    /// Returns `None` when the ring buffer is closed.
    pub fn recv(&mut self) -> Option<T> {
        loop {
            match self.try_recv() {
                Ok(v) => return Some(v),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return None,
                Err(RecvError::Empty) => std::hint::spin_loop(),
            }
        }
    }

    /// Async receive. Suspends the task (via `Notify`) when the ring buffer is
    /// empty. Returns the event and any lag count, or `None` when closed.
    ///
    /// # Notify correctness
    /// `notified().enable()` is called *before* each `try_recv` so that any
    /// `notify_waiters()` fired by the publisher between the empty check and
    /// the `.await` is not missed.
    pub async fn recv_async(&mut self) -> Result<T, RecvError> {
        loop {
            // Clone the Arc so `notified` does not borrow `self`, allowing
            // the mutable `self.try_recv()` call below to compile.
            let inner = Arc::clone(&self.inner);
            let notified = inner.notify.notified();
            tokio::pin!(notified);
            // Register as a waiter before polling; prevents missed notifications
            // if the publisher fires between the empty check and the .await.
            notified.as_mut().enable();

            match self.try_recv() {
                Ok(v) => return Ok(v),
                Err(RecvError::Lagged(n)) => return Err(RecvError::Lagged(n)),
                Err(RecvError::Closed) => return Err(RecvError::Closed),
                Err(RecvError::Empty) => notified.await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct TrackDrop {
        counter: Arc<AtomicUsize>,
    }

    impl TrackDrop {
        fn new(counter: Arc<AtomicUsize>) -> Self {
            counter.fetch_add(1, Ordering::SeqCst);
            Self { counter }
        }
    }

    impl Clone for TrackDrop {
        fn clone(&self) -> Self {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Self { counter: self.counter.clone() }
        }
    }

    impl Drop for TrackDrop {
        fn drop(&mut self) {
            self.counter.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn test_drop_on_overwrite() {
        let counter = Arc::new(AtomicUsize::new(0));
        let buf = RingBuffer::new(4);
        assert_eq!(buf.capacity(), 4);

        for _ in 0..4 {
            buf.publish(TrackDrop::new(counter.clone()));
        }
        assert_eq!(counter.load(Ordering::SeqCst), 4);

        // Overwrite the first slot
        buf.publish(TrackDrop::new(counter.clone()));
        assert_eq!(counter.load(Ordering::SeqCst), 4);

        // Overwrite the second slot
        buf.publish(TrackDrop::new(counter.clone()));
        assert_eq!(counter.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn test_drop_on_buffer_drop() {
        let counter = Arc::new(AtomicUsize::new(0));
        {
            let buf = RingBuffer::new(4);
            for _ in 0..3 {
                buf.publish(TrackDrop::new(counter.clone()));
            }
            assert_eq!(counter.load(Ordering::SeqCst), 3);
        }
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_drop_on_buffer_drop_full() {
        let counter = Arc::new(AtomicUsize::new(0));
        {
            let buf = RingBuffer::new(4);
            for _ in 0..10 {
                buf.publish(TrackDrop::new(counter.clone()));
            }
            assert_eq!(counter.load(Ordering::SeqCst), 4);
        }
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_basic_publish_subscribe() {
        let buf = RingBuffer::new(4);
        let mut sub = buf.subscribe();

        assert_eq!(sub.try_recv(), Err(RecvError::Empty));

        buf.publish(10);
        buf.publish(20);

        assert_eq!(sub.try_recv(), Ok(10));
        assert_eq!(sub.try_recv(), Ok(20));
        assert_eq!(sub.try_recv(), Err(RecvError::Empty));
    }

    #[test]
    fn test_subscriber_lagged() {
        let buf = RingBuffer::new(4);
        let mut sub = buf.subscribe();

        // Publish 6 items. Capacity is 4.
        // Slots will be overwritten.
        // Seq 0, 1, 2, 3, 4, 5.
        // Slot 0 gets 0 then 4.
        // Slot 1 gets 1 then 5.
        // Slot 2 gets 2.
        // Slot 3 gets 3.
        for i in 0..6 {
            buf.publish(i);
        }

        // Sub cursor is 0.
        // Slot 0 now has sequence 5 (committed seq for item 4, which is 5).
        // Since slot 0's sequence (5) > sub cursor + 1 (1), the sub is lagged.
        // It will skip forward to lapped_to = seq - 1 = 4.
        // Skipped count = lapped_to - next = 4 - 0 = 4.
        assert_eq!(sub.try_recv(), Err(RecvError::Lagged(4)));
        assert_eq!(sub.cursor, 4);

        // Next read should get sequence 4 (which is item 4).
        assert_eq!(sub.try_recv(), Ok(4));
        assert_eq!(sub.try_recv(), Ok(5));
        assert_eq!(sub.try_recv(), Err(RecvError::Empty));
    }

    #[test]
    fn test_buffer_closed() {
        let buf = RingBuffer::new(4);
        let mut sub = buf.subscribe();

        buf.publish(1);
        buf.close();

        assert_eq!(sub.try_recv(), Ok(1));
        assert_eq!(sub.try_recv(), Err(RecvError::Closed));
        assert_eq!(sub.recv(), None);
    }

    #[test]
    fn test_async_recv() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let buf = RingBuffer::new(4);
            let mut sub = buf.subscribe();

            let handle = tokio::spawn(async move {
                sub.recv_async().await
            });

            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            buf.publish(42);

            let res = handle.await.unwrap();
            assert_eq!(res, Ok(42));
        });
    }
}
