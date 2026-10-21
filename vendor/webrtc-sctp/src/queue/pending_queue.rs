use std::collections::VecDeque;
use std::sync::atomic::Ordering;

use portable_atomic::{AtomicBool, AtomicUsize};
use tokio::sync::{Mutex, Semaphore};
use util::sync::RwLock;

use crate::chunk::chunk_payload_data::ChunkPayloadData;

// TODO: benchmark performance between multiple Atomic+Mutex vs one Mutex<PendingQueueInternal>

// Some tests push a lot of data before starting to process any data...
#[cfg(test)]
const QUEUE_BYTES_LIMIT: usize = 128 * 1024 * 1024;
/// Maximum size of the pending queue, in bytes.
#[cfg(not(test))]
const QUEUE_BYTES_LIMIT: usize = 128 * 1024;

/// Basic queue for either ordered or unordered chunks.
pub(crate) type PendingBaseQueue = VecDeque<ChunkPayloadData>;

/// A queue for both ordered and unordered chunks.
#[derive(Debug)]
pub(crate) struct PendingQueue {
    // These two fields limit appending bytes to the queue
    // This two step process is necessary because
    // A) We need backpressure which the semaphore applies by limiting the total amount of bytes via the permits
    // B) The chunks of one fragmented message need to be put in direct sequence into the queue which the lock guarantees
    //
    // The semaphore is not inside the lock because the permits need to be returned without needing a lock on the semaphore
    semaphore_lock: Mutex<()>,
    semaphore: Semaphore,
    /// Larger messages enter in fragments so a bounded queue can make progress.
    append_large_threshold: usize,

    unordered_queue: RwLock<PendingBaseQueue>,
    ordered_queue: RwLock<PendingBaseQueue>,
    queue_len: AtomicUsize,
    n_bytes: AtomicUsize,
    selected: AtomicBool,
    unordered_is_selected: AtomicBool,
}

impl Default for PendingQueue {
    fn default() -> Self {
        PendingQueue::new()
    }
}

impl PendingQueue {
    pub(crate) fn new() -> Self {
        Self::with_capacity(QUEUE_BYTES_LIMIT)
    }

    // Allow backpressure tests to use the production byte limit instead of the
    // enlarged default needed by older tests that enqueue before consuming.
    pub(crate) fn with_capacity(byte_limit: usize) -> Self {
        Self {
            semaphore_lock: Mutex::default(),
            semaphore: Semaphore::new(byte_limit),
            append_large_threshold: (byte_limit * 2) / 3,
            unordered_queue: Default::default(),
            ordered_queue: Default::default(),
            queue_len: Default::default(),
            n_bytes: Default::default(),
            selected: Default::default(),
            unordered_is_selected: Default::default(),
        }
    }

    /// Appends a chunk to the back of the pending queue.
    pub(crate) async fn push(&self, c: ChunkPayloadData) {
        let user_data_len = c.user_data.len();

        let _sem_lock = self.semaphore_lock.lock().await;
        let permits = self.semaphore.acquire_many(user_data_len as u32).await;
        // unwrap ok because we never close the semaphore unless we have dropped self
        permits.unwrap().forget();
        self.push_unlimited(c);
    }

    /// Appends chunks to the back of the pending queue.
    ///
    /// # Panics
    ///
    /// If it's a mix of unordered and ordered chunks.
    pub(crate) async fn append(&self, chunks: Vec<ChunkPayloadData>, on_ready: impl Fn()) {
        if chunks.is_empty() {
            return;
        }

        let total_user_data_len = chunks.iter().fold(0, |acc, c| acc + c.user_data.len());

        if total_user_data_len >= self.append_large_threshold {
            self.append_large(chunks, &on_ready).await;
        } else {
            let _sem_lock = self.semaphore_lock.lock().await;
            let permits = self
                .semaphore
                .acquire_many(total_user_data_len as u32)
                .await;
            // unwrap ok because we never close the semaphore unless we have dropped self
            permits.unwrap().forget();
            self.append_unlimited(chunks, total_user_data_len);
        }
        on_ready();
    }

    // If this is a very large message we append chunks one by one to allow progress while we are appending
    async fn append_large(&self, chunks: Vec<ChunkPayloadData>, on_ready: &impl Fn()) {
        // lock this for the whole duration
        let _sem_lock = self.semaphore_lock.lock().await;

        for chunk in chunks.into_iter() {
            let user_data_len = chunk.user_data.len();
            let permits = match self.semaphore.try_acquire_many(user_data_len as u32) {
                Ok(permits) => permits,
                Err(_) => {
                    // The queue already contains work. Waiting for the entire
                    // append before waking an idle writer deadlocks whenever
                    // a message is larger than the byte limit. Wake before
                    // waiting, retaining the lock so fragments stay contiguous.
                    on_ready();
                    self.semaphore
                        .acquire_many(user_data_len as u32)
                        .await
                        .expect("pending queue semaphore is never closed")
                }
            };
            permits.forget();
            self.push_unlimited(chunk);
        }
    }

    // Account under the queue lock: a woken writer must never pop a published
    // fragment before its bytes and length have been added to the counters.
    fn push_unlimited(&self, chunk: ChunkPayloadData) {
        let user_data_len = chunk.user_data.len();
        let mut queue = if chunk.unordered {
            self.unordered_queue.write()
        } else {
            self.ordered_queue.write()
        };
        queue.push_back(chunk);
        self.n_bytes.fetch_add(user_data_len, Ordering::SeqCst);
        self.queue_len.fetch_add(1, Ordering::SeqCst);
    }

    /// Assumes that A) enough permits have been acquired and forget from the semaphore and that the semaphore_lock is held
    fn append_unlimited(&self, chunks: Vec<ChunkPayloadData>, total_user_data_len: usize) {
        let chunks_len = chunks.len();
        let unordered = chunks
            .first()
            .expect("chunks to not be empty because of the above check")
            .unordered;
        assert!(
            chunks.iter().all(|c| c.unordered == unordered),
            "expected all chunks to have the same ordering"
        );
        let mut queue = if unordered {
            self.unordered_queue.write()
        } else {
            self.ordered_queue.write()
        };
        queue.extend(chunks);

        self.n_bytes
            .fetch_add(total_user_data_len, Ordering::SeqCst);
        self.queue_len.fetch_add(chunks_len, Ordering::SeqCst);
    }

    pub(crate) fn peek(&self) -> Option<ChunkPayloadData> {
        if self.selected.load(Ordering::SeqCst) {
            if self.unordered_is_selected.load(Ordering::SeqCst) {
                let unordered_queue = self.unordered_queue.read();
                return unordered_queue.front().cloned();
            } else {
                let ordered_queue = self.ordered_queue.read();
                return ordered_queue.front().cloned();
            }
        }

        let c = {
            let unordered_queue = self.unordered_queue.read();
            unordered_queue.front().cloned()
        };

        if c.is_some() {
            return c;
        }

        let ordered_queue = self.ordered_queue.read();
        ordered_queue.front().cloned()
    }

    pub(crate) fn pop(
        &self,
        beginning_fragment: bool,
        unordered: bool,
    ) -> Option<ChunkPayloadData> {
        let popped = if self.selected.load(Ordering::SeqCst) {
            let popped = if self.unordered_is_selected.load(Ordering::SeqCst) {
                let mut unordered_queue = self.unordered_queue.write();
                unordered_queue.pop_front()
            } else {
                let mut ordered_queue = self.ordered_queue.write();
                ordered_queue.pop_front()
            };
            if let Some(p) = &popped {
                if p.ending_fragment {
                    self.selected.store(false, Ordering::SeqCst);
                }
            }
            popped
        } else {
            if !beginning_fragment {
                return None;
            }
            if unordered {
                let popped = {
                    let mut unordered_queue = self.unordered_queue.write();
                    unordered_queue.pop_front()
                };
                if let Some(p) = &popped {
                    if !p.ending_fragment {
                        self.selected.store(true, Ordering::SeqCst);
                        self.unordered_is_selected.store(true, Ordering::SeqCst);
                    }
                }
                popped
            } else {
                let popped = {
                    let mut ordered_queue = self.ordered_queue.write();
                    ordered_queue.pop_front()
                };
                if let Some(p) = &popped {
                    if !p.ending_fragment {
                        self.selected.store(true, Ordering::SeqCst);
                        self.unordered_is_selected.store(false, Ordering::SeqCst);
                    }
                }
                popped
            }
        };

        if let Some(p) = &popped {
            let user_data_len = p.user_data.len();
            self.n_bytes.fetch_sub(user_data_len, Ordering::SeqCst);
            self.queue_len.fetch_sub(1, Ordering::SeqCst);
            self.semaphore.add_permits(user_data_len);
        }

        popped
    }

    pub(crate) fn get_num_bytes(&self) -> usize {
        self.n_bytes.load(Ordering::SeqCst)
    }

    pub(crate) fn len(&self) -> usize {
        self.queue_len.load(Ordering::SeqCst)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
