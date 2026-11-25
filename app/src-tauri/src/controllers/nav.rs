//! Controller input -> navigation actions for the UI (`pad-nav`): D-pad or left stick move the focus (with a
//! repeat while held), face and shoulder buttons act once per press. Pure: the time comes in from the caller.

use super::{PadState, btn};
use serde::Serialize;
use std::time::{Duration, Instant};

/// A held direction repeats after this long...
pub const REPEAT_DELAY: Duration = Duration::from_millis(400);
/// ...every this long...
pub const REPEAT_EVERY: Duration = Duration::from_millis(110);
/// ...and faster after this many repeats.
pub const FAST_AFTER: u32 = 6;
pub const REPEAT_FAST: Duration = Duration::from_millis(60);
/// The stick counts as pushed past this (of full travel) and as let go below `STICK_OFF` (hysteresis, so a
/// stick resting near the edge does not flicker).
pub const STICK_ON: f32 = 0.55;
pub const STICK_OFF: f32 = 0.35;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    /// A / Cross (B on a Nintendo pad: the right button).
    Confirm,
    /// B / Circle.
    Back,
    /// X / Square: the second action (details).
    Alt,
    /// Y / Triangle.
    Search,
    /// Start / Options: the game's menu.
    Menu,
    /// LB / L1, RB / R1: the previous and next page.
    Prev,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    fn action(self) -> Action {
        match self {
            Dir::Up => Action::Up,
            Dir::Down => Action::Down,
            Dir::Left => Action::Left,
            Dir::Right => Action::Right,
        }
    }
}

fn dpad(buttons: u32) -> Option<Dir> {
    [(btn::UP, Dir::Up), (btn::DOWN, Dir::Down), (btn::LEFT, Dir::Left), (btn::RIGHT, Dir::Right)]
        .into_iter()
        .find(|(b, _)| buttons & b != 0)
        .map(|(_, d)| d)
}

/// The left stick's direction (y grows downwards). `held`: the direction it gave last time, kept until the
/// stick falls back under `STICK_OFF` on that axis.
fn stick(x: f32, y: f32, held: Option<Dir>) -> Option<Dir> {
    let along = |d: Dir| match d {
        Dir::Up => -y,
        Dir::Down => y,
        Dir::Left => -x,
        Dir::Right => x,
    };
    let across = |d: Dir| match d {
        Dir::Up | Dir::Down => x.abs(),
        Dir::Left | Dir::Right => y.abs(),
    };
    if let Some(d) = held
        && along(d) >= STICK_OFF
        && along(d) >= across(d)
    {
        return Some(d);
    }
    if x.abs().max(y.abs()) < STICK_ON {
        return None;
    }
    Some(if x.abs() > y.abs() {
        if x < 0.0 { Dir::Left } else { Dir::Right }
    } else if y < 0.0 {
        Dir::Up
    } else {
        Dir::Down
    })
}

/// One pad's navigation state.
#[derive(Debug, Clone)]
pub struct Nav {
    prev: u32,
    stick: Option<Dir>,
    held: Option<Dir>,
    next_at: Option<Instant>,
    repeats: u32,
}

impl Nav {
    /// Start from what the pad shows now: buttons already down (pressed in a game, say) do nothing until they
    /// are let go and pressed again.
    pub fn new(start: &PadState) -> Nav {
        Nav { prev: start.buttons, stick: stick(start.lx, start.ly, None), held: None, next_at: None, repeats: 0 }
            .holding_start(start)
    }

    fn holding_start(mut self, s: &PadState) -> Nav {
        // A direction held at the start does not move until it is let go.
        self.held = dpad(s.buttons).or(self.stick);
        self
    }

    /// The pad's new state. `confirm_east`: the right face button confirms (Nintendo pads), else the bottom one.
    pub fn update(&mut self, s: &PadState, confirm_east: bool, now: Instant) -> Vec<Action> {
        let mut out = Vec::new();
        let pressed = s.buttons & !self.prev;
        self.prev = s.buttons;
        self.stick = stick(s.lx, s.ly, self.stick);
        let dir = dpad(s.buttons).or(self.stick);
        if dir != self.held {
            self.held = dir;
            self.repeats = 0;
            self.next_at = dir.map(|_| now + REPEAT_DELAY);
            if let Some(d) = dir {
                out.push(d.action());
            }
        }
        let (confirm, back) = if confirm_east { (btn::EAST, btn::SOUTH) } else { (btn::SOUTH, btn::EAST) };
        for (b, a) in [
            (confirm, Action::Confirm),
            (back, Action::Back),
            (btn::WEST, Action::Alt),
            (btn::NORTH, Action::Search),
            (btn::START, Action::Menu),
            (btn::LB, Action::Prev),
            (btn::RB, Action::Next),
        ] {
            if pressed & b != 0 {
                out.push(a);
            }
        }
        out
    }

    /// When the held direction repeats next.
    pub fn deadline(&self) -> Option<Instant> {
        self.held.and(self.next_at)
    }

    /// The repeat of a held direction, if it is due.
    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        let (d, at) = (self.held?, self.next_at?);
        if now < at {
            return None;
        }
        self.repeats += 1;
        let every = if self.repeats >= FAST_AFTER { REPEAT_FAST } else { REPEAT_EVERY };
        self.next_at = Some(at.max(now - every) + every);
        Some(d.action())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(buttons: u32, lx: f32, ly: f32) -> PadState {
        PadState { buttons, lx, ly, ..PadState::default() }
    }

    #[test]
    fn buttons_act_once_per_press() {
        let t = Instant::now();
        let mut n = Nav::new(&st(0, 0.0, 0.0));
        assert_eq!(n.update(&st(btn::SOUTH, 0.0, 0.0), false, t), [Action::Confirm]);
        assert!(n.update(&st(btn::SOUTH, 0.0, 0.0), false, t).is_empty(), "held: nothing more");
        assert!(n.update(&st(0, 0.0, 0.0), false, t).is_empty());
        assert_eq!(n.update(&st(btn::EAST | btn::START, 0.0, 0.0), false, t), [Action::Back, Action::Menu]);
        assert_eq!(n.update(&st(btn::LB | btn::RB | btn::NORTH | btn::WEST, 0.0, 0.0), false, t), [Action::Alt, Action::Search, Action::Prev, Action::Next]);
    }

    #[test]
    fn nintendo_layout_confirms_with_the_right_button() {
        let t = Instant::now();
        let mut n = Nav::new(&st(0, 0.0, 0.0));
        assert_eq!(n.update(&st(btn::EAST, 0.0, 0.0), true, t), [Action::Confirm]);
        assert_eq!(n.update(&st(btn::SOUTH, 0.0, 0.0), true, t), [Action::Back]);
    }

    #[test]
    fn buttons_down_at_the_start_wait_for_a_new_press() {
        let t = Instant::now();
        let mut n = Nav::new(&st(btn::SOUTH | btn::RIGHT, 0.0, 0.0));
        assert!(n.update(&st(btn::SOUTH | btn::RIGHT, 0.0, 0.0), false, t).is_empty());
        assert_eq!(n.tick(t + Duration::from_secs(5)), None, "a direction held from before does not repeat");
        assert!(n.update(&st(0, 0.0, 0.0), false, t).is_empty());
        assert_eq!(n.update(&st(btn::SOUTH, 0.0, 0.0), false, t), [Action::Confirm]);
    }

    #[test]
    fn stick_deadzone_and_hysteresis() {
        let t = Instant::now();
        let mut n = Nav::new(&st(0, 0.0, 0.0));
        assert!(n.update(&st(0, 0.4, 0.1), false, t).is_empty(), "inside the deadzone");
        assert_eq!(n.update(&st(0, 0.7, 0.1), false, t), [Action::Right]);
        assert!(n.update(&st(0, 0.4, 0.1), false, t).is_empty(), "still held above STICK_OFF");
        assert!(n.update(&st(0, 0.2, 0.0), false, t).is_empty(), "let go");
        assert_eq!(n.update(&st(0, 0.1, -0.9), false, t), [Action::Up], "dominant axis");
        assert_eq!(n.update(&st(0, -0.9, -0.1), false, t), [Action::Left]);
    }

    #[test]
    fn a_held_direction_repeats_then_speeds_up() {
        let t0 = Instant::now();
        let mut n = Nav::new(&st(0, 0.0, 0.0));
        assert_eq!(n.update(&st(btn::DOWN, 0.0, 0.0), false, t0), [Action::Down]);
        assert_eq!(n.deadline(), Some(t0 + REPEAT_DELAY));
        assert_eq!(n.tick(t0 + REPEAT_DELAY - Duration::from_millis(1)), None);
        let mut t = t0 + REPEAT_DELAY;
        let mut gaps = Vec::new();
        for _ in 0..8 {
            assert_eq!(n.tick(t), Some(Action::Down));
            let next = n.deadline().unwrap();
            gaps.push(next - t);
            t = next;
        }
        assert_eq!(gaps[0], REPEAT_EVERY);
        assert_eq!(*gaps.last().unwrap(), REPEAT_FAST);
        // Let go: no more repeats.
        assert!(n.update(&st(0, 0.0, 0.0), false, t).is_empty());
        assert_eq!(n.deadline(), None);
        assert_eq!(n.tick(t + Duration::from_secs(1)), None);
    }

    #[test]
    fn dpad_wins_over_the_stick_and_a_new_direction_moves_at_once() {
        let t = Instant::now();
        let mut n = Nav::new(&st(0, 0.0, 0.0));
        assert_eq!(n.update(&st(btn::LEFT, 0.9, 0.0), false, t), [Action::Left]);
        assert_eq!(n.update(&st(0, 0.9, 0.0), false, t), [Action::Right]);
    }
}
