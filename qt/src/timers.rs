//! Messages delivered after a delay (debounces: sidecar writes, settle ticks, hover rests) by one thread.

use std::collections::BinaryHeap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::app::Msg;
use crate::bridge::Tx;

struct Due(Instant, u64, Msg);

impl PartialEq for Due {
    fn eq(&self, o: &Self) -> bool {
        (self.0, self.1) == (o.0, o.1)
    }
}
impl Eq for Due {}
impl PartialOrd for Due {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Due {
    /// Earliest first in a max-heap; equal times in the order they were set.
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        (o.0, o.1).cmp(&(self.0, self.1))
    }
}

#[derive(Clone)]
pub struct Timers(mpsc::Sender<(Duration, Msg)>);

impl Timers {
    pub fn new(tx: Tx) -> Self {
        let (send, recv) = mpsc::channel::<(Duration, Msg)>();
        std::thread::spawn(move || {
            let (mut heap, mut n) = (BinaryHeap::new(), 0u64);
            loop {
                let wait = heap.peek().map(|d: &Due| d.0.saturating_duration_since(Instant::now()));
                let got = match wait {
                    Some(w) => recv.recv_timeout(w),
                    None => recv.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                };
                match got {
                    Ok((after, msg)) => {
                        n += 1;
                        heap.push(Due(Instant::now() + after, n, msg));
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
                while heap.peek().is_some_and(|d| d.0 <= Instant::now()) {
                    tx.send(heap.pop().expect("peeked").2);
                }
            }
        });
        Self(send)
    }

    pub fn after(&self, delay: Duration, msg: Msg) {
        let _ = self.0.send((delay, msg));
    }
}
