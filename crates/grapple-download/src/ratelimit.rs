//! One token bucket shared by every download worker (the "Speed limit" setting). 0 = unlimited.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub struct RateLimit {
    rate: AtomicU64,
    /// (tokens, last refill). Tokens may go negative: a taker then sleeps off its debt, so several workers together
    /// never pass the rate.
    state: Mutex<(f64, Instant)>,
}

impl RateLimit {
    pub fn new(bytes_per_sec: u64) -> RateLimit {
        RateLimit { rate: AtomicU64::new(bytes_per_sec), state: Mutex::new((0.0, Instant::now())) }
    }

    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    pub fn set_rate(&self, bytes_per_sec: u64) {
        self.rate.store(bytes_per_sec, Ordering::Relaxed);
        *self.state.lock().unwrap() = (0.0, Instant::now());
    }

    /// Account for `n` bytes; waits when over the limit.
    pub async fn take(&self, n: usize) {
        let rate = self.rate();
        if rate == 0 {
            return;
        }
        let wait = {
            let mut s = self.state.lock().unwrap();
            let now = Instant::now();
            // A small burst (1/10 s) keeps the stream smooth without letting it run ahead.
            let burst = (rate as f64 / 10.0).max(16.0 * 1024.0);
            s.0 = (s.0 + now.duration_since(s.1).as_secs_f64() * rate as f64).min(burst);
            s.1 = now;
            s.0 -= n as f64;
            if s.0 < 0.0 { Some(Duration::from_secs_f64(-s.0 / rate as f64)) } else { None }
        };
        if let Some(w) = wait {
            tokio::time::sleep(w).await;
        }
    }
}
