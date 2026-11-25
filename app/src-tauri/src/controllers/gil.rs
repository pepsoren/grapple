//! Windows and macOS: pads through gilrs (Windows.Gaming.Input / XInput, IOKit). Buttons come out by position
//! (gilrs applies SDL's controller mappings). gilrs cannot sleep until a pad does something, so it runs only
//! while grapple's window is on screen; it is dropped (and every pad with it) while the window is hidden.

use super::{Changes, Connection, PadState, RawPad, Rumble, Source, Waker, btn, clean_name};
use gilrs::ff::{BaseEffect, BaseEffectType, Effect, EffectBuilder, Replay, Ticks};
use gilrs::{Axis, Button, EventType, Gamepad, GamepadId, Gilrs, PowerInfo};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long one `next_event_blocking` may wait, so commands are never held up for long.
const SLICE_READING: Duration = Duration::from_millis(15);
const SLICE_WATCHING: Duration = Duration::from_millis(250);

pub struct Gil {
    g: Option<Gilrs>,
    reading: bool,
    effects: HashMap<GamepadId, Effect>,
    wake: Arc<(Mutex<bool>, Condvar)>,
}

impl Gil {
    pub fn new() -> Gil {
        Gil { g: Gilrs::new().ok(), reading: false, effects: HashMap::new(), wake: Arc::new((Mutex::new(false), Condvar::new())) }
    }
}

fn id_of(id: GamepadId) -> String {
    format!("g{}", usize::from(id))
}

fn state(p: &Gamepad<'_>) -> PadState {
    let mut buttons = 0;
    for (b, bit) in [
        (Button::South, btn::SOUTH),
        (Button::East, btn::EAST),
        (Button::West, btn::WEST),
        (Button::North, btn::NORTH),
        (Button::LeftTrigger, btn::LB),
        (Button::RightTrigger, btn::RB),
        (Button::LeftTrigger2, btn::LT),
        (Button::RightTrigger2, btn::RT),
        (Button::Select, btn::SELECT),
        (Button::Start, btn::START),
        (Button::Mode, btn::GUIDE),
        (Button::LeftThumb, btn::LS),
        (Button::RightThumb, btn::RS),
        (Button::DPadUp, btn::UP),
        (Button::DPadDown, btn::DOWN),
        (Button::DPadLeft, btn::LEFT),
        (Button::DPadRight, btn::RIGHT),
    ] {
        if p.is_pressed(b) {
            buttons |= bit;
        }
    }
    let trig = |b: Button| p.button_data(b).map(|d| d.value()).unwrap_or(0.0);
    PadState {
        buttons,
        lx: p.value(Axis::LeftStickX),
        // gilrs: up is positive.
        ly: -p.value(Axis::LeftStickY),
        rx: p.value(Axis::RightStickX),
        ry: -p.value(Axis::RightStickY),
        lt: trig(Button::LeftTrigger2),
        rt: trig(Button::RightTrigger2),
    }
}

impl Source for Gil {
    fn pads(&mut self) -> Vec<RawPad> {
        let Some(g) = &self.g else { return Vec::new() };
        g.gamepads()
            .map(|(id, p)| {
                let (connection, battery, charging) = match p.power_info() {
                    PowerInfo::Wired => (Some(Connection::Wired), None, false),
                    PowerInfo::Discharging(b) => (None, Some(b), false),
                    PowerInfo::Charging(b) => (None, Some(b), true),
                    PowerInfo::Charged => (None, Some(100), true),
                    PowerInfo::Unknown => (None, None, false),
                };
                RawPad {
                    id: id_of(id),
                    name: clean_name(p.name()),
                    vendor: p.vendor_id().unwrap_or(0),
                    connection,
                    battery,
                    charging,
                    rumble: p.is_ff_supported(),
                }
            })
            .collect()
    }

    fn set_mode(&mut self, read: bool, watch: bool) {
        self.reading = read;
        if read || watch {
            if self.g.is_none() {
                self.g = Gilrs::new().ok();
            }
        } else if self.g.is_some() {
            self.effects.clear();
            self.g = None;
        }
    }

    fn wait(&mut self, timeout: Duration) -> Changes {
        let mut out = Changes::default();
        let Some(g) = &mut self.g else {
            let (lock, cv) = &*self.wake;
            let woken = lock.lock().unwrap();
            let mut woken = cv.wait_timeout_while(woken, timeout, |w| !*w).unwrap().0;
            *woken = false;
            return out;
        };
        let end = Instant::now() + timeout;
        let slice = if self.reading { SLICE_READING } else { SLICE_WATCHING };
        let mut touched: Vec<GamepadId> = Vec::new();
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let first = g.next_event_blocking(Some(left.min(slice)));
            let mut ev = first;
            while let Some(e) = ev {
                match e.event {
                    EventType::Connected | EventType::Disconnected => out.list = true,
                    _ if self.reading => {
                        if !touched.contains(&e.id) {
                            touched.push(e.id);
                        }
                    }
                    _ => {}
                }
                ev = g.next_event();
            }
            let woken = std::mem::take(&mut *self.wake.0.lock().unwrap());
            if out.list || !touched.is_empty() || woken || Instant::now() >= end {
                break;
            }
        }
        for id in touched {
            if let Some(p) = g.connected_gamepad(id) {
                out.states.push((id_of(id), state(&p)));
            }
        }
        out
    }

    fn rumble(&mut self, id: &str, r: Rumble) -> bool {
        let Some(g) = &mut self.g else { return false };
        let Some(gid) = g.gamepads().find(|(gid, p)| id_of(*gid) == id && p.is_ff_supported()).map(|(gid, _)| gid) else { return false };
        let (strong, weak, ms) = r.params();
        let replay = Replay { play_for: Ticks::from_ms(ms as u32), ..Default::default() };
        let effect = EffectBuilder::new()
            .add_effect(BaseEffect { kind: BaseEffectType::Strong { magnitude: strong }, scheduling: replay, ..Default::default() })
            .add_effect(BaseEffect { kind: BaseEffectType::Weak { magnitude: weak }, scheduling: replay, ..Default::default() })
            .gamepads(&[gid])
            .finish(g);
        match effect {
            Ok(e) => {
                let ok = e.play().is_ok();
                // Dropping the effect stops it: keep it until the next one.
                self.effects.insert(gid, e);
                ok
            }
            Err(_) => false,
        }
    }

    fn snapshot(&mut self) -> Vec<(String, PadState)> {
        match (&self.g, self.reading) {
            (Some(g), true) => g.gamepads().map(|(id, p)| (id_of(id), state(&p))).collect(),
            _ => Vec::new(),
        }
    }

    fn waker(&self) -> Waker {
        let w = self.wake.clone();
        Arc::new(move || {
            *w.0.lock().unwrap() = true;
            w.1.notify_all();
        })
    }
}
