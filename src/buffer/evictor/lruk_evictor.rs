use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::buffer::evictor::{self, EvictionPolicy};

struct FrameInfo {
    /// Stores up to K logical timestamps of accesses
    history: VecDeque<usize>,
    evictable: bool,
}

pub struct LruKEvictor {
    k: usize,
    // Logical clock to track access recency without the overhead of SystemTime
    current_timestamp: AtomicUsize,
    state: Mutex<HashMap<usize, FrameInfo>>,
}

impl LruKEvictor {
    pub fn new(k: usize) -> Self {
        Self {
            k,
            current_timestamp: AtomicUsize::new(0),
            state: Mutex::new(HashMap::new()),
        }
    }
}

impl EvictionPolicy for LruKEvictor {
    fn find_victim(&self) -> evictor::Result<usize> {
        let state = self.state.lock().unwrap();

        let mut victim = None;
        let mut max_backward_k_dist = 0;
        let mut earliest_access_for_inf = usize::MAX;

        let current_ts = self.current_timestamp.load(Ordering::SeqCst);

        for (&frame_id, info) in state.iter() {
            if !info.evictable || info.history.is_empty() {
                continue;
            }

            if info.history.len() < self.k {
                // Rule 1: Pages with < K accesses have +infinity backward distance.
                // We tie-break by evicting the one with the earliest overall access (FIFO).
                let first_access = *info.history.front().unwrap();
                if first_access < earliest_access_for_inf {
                    earliest_access_for_inf = first_access;
                    victim = Some(frame_id);
                    max_backward_k_dist = usize::MAX; // +infinity lock
                }
            } else if max_backward_k_dist < usize::MAX {
                // Rule 2: Pages with exactly K accesses.
                // Evict the one whose K-th most recent access is the oldest.
                let kth_access = *info.history.front().unwrap();
                let distance = current_ts.saturating_sub(kth_access);

                if distance > max_backward_k_dist {
                    max_backward_k_dist = distance;
                    victim = Some(frame_id);
                }
            }
        }

        victim.ok_or(evictor::Error::NoVictimFound)
    }

    fn record_access(&self, frame_id: usize) {
        let mut state = self.state.lock().unwrap();
        let ts = self.current_timestamp.fetch_add(1, Ordering::SeqCst);

        let info = state.entry(frame_id).or_insert(FrameInfo {
            history: VecDeque::with_capacity(self.k),
            evictable: false, // Assume safely un-evictable until explicitly allowed
        });

        info.history.push_back(ts);

        // Truncate history to keep only the last K accesses
        if info.history.len() > self.k {
            info.history.pop_front();
        }
    }

    fn set_evictable(&self, frame_id: usize, evictable: bool) {
        let mut state = self.state.lock().unwrap();

        // If the frame doesn't exist in the evictor yet, initialize it
        let info = state.entry(frame_id).or_insert(FrameInfo {
            history: VecDeque::with_capacity(self.k),
            evictable,
        });

        info.evictable = evictable;
    }

    fn remove_frame(&self, frame_id: usize) {
        let mut state = self.state.lock().unwrap();
        state.remove(&frame_id);
    }
}
