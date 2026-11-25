//! MOCK: two sample pads for the UI-only build, with input that moves by itself while the Test panel is open.
//! Its input never navigates (see [`Source::synthetic`]).

use super::{Changes, Connection, PadState, RawPad, Rumble, Source, Waker, btn};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub struct Mock {
    start: Instant,
    reading: bool,
    wake: Arc<(Mutex<bool>, Condvar)>,
}

impl Mock {
    pub fn new() -> Mock {
        Mock { start: Instant::now(), reading: false, wake: Arc::new((Mutex::new(false), Condvar::new())) }
    }
}

/// The sample input at `t` seconds: sticks circle, the triggers breathe, one button after another lights.
pub fn sample(t: f32) -> PadState {
    const CYCLE: [u32; 8] = [btn::SOUTH, btn::EAST, btn::NORTH, btn::WEST, btn::UP, btn::RIGHT, btn::DOWN, btn::LEFT];
    PadState {
        buttons: CYCLE[(t / 0.6) as usize % CYCLE.len()],
        lx: (t * 1.7).cos() * 0.8,
        ly: (t * 1.7).sin() * 0.8,
        rx: (t * 0.9).sin() * 0.5,
        ry: 0.0,
        lt: ((t * 1.3).sin() + 1.0) / 2.0,
        rt: ((t * 0.7).cos() + 1.0) / 4.0,
    }
}

impl Source for Mock {
    fn pads(&mut self) -> Vec<RawPad> {
        vec![
            RawPad {
                id: "054c:0ce6:mock".into(),
                name: "DualSense Wireless Controller".into(),
                vendor: 0x054c,
                connection: Some(Connection::Usb),
                battery: None,
                charging: false,
                rumble: true,
            },
            RawPad {
                id: "045e:0b13:mock".into(),
                name: "Xbox Wireless Controller".into(),
                vendor: 0x045e,
                connection: Some(Connection::Bluetooth),
                battery: Some(72),
                charging: false,
                rumble: true,
            },
        ]
    }

    fn set_mode(&mut self, read: bool, _watch: bool) {
        self.reading = read;
    }

    fn wait(&mut self, timeout: Duration) -> Changes {
        let step = if self.reading { timeout.min(Duration::from_millis(16)) } else { timeout };
        {
            let (lock, cv) = &*self.wake;
            let woken = lock.lock().unwrap();
            let mut woken = cv.wait_timeout_while(woken, step, |w| !*w).unwrap().0;
            *woken = false;
        }
        let mut out = Changes::default();
        if self.reading {
            let s = sample(self.start.elapsed().as_secs_f32());
            out.states = self.pads().into_iter().map(|p| (p.id, s)).collect();
        }
        out
    }

    fn rumble(&mut self, _id: &str, _r: Rumble) -> bool {
        true
    }

    fn snapshot(&mut self) -> Vec<(String, PadState)> {
        Vec::new()
    }

    fn waker(&self) -> Waker {
        let w = self.wake.clone();
        Arc::new(move || {
            *w.0.lock().unwrap() = true;
            w.1.notify_all();
        })
    }

    fn synthetic(&self) -> bool {
        true
    }
}
