//! "Nothing while I'm playing" (Settings › Notifications): desktop notifications that arrive while a game runs wait
//! here and go out when the last game closes. Every notification grapple sends passes [`Gate::submit`].

use std::sync::Mutex;

/// At most this many held notifications are kept (the oldest go first): a long session must not end in a flood.
const HELD_MAX: usize = 5;

#[derive(Debug, Default)]
pub struct Gate {
    on: bool,
    playing: bool,
    held: Vec<(String, String)>,
}

impl Gate {
    /// The setting changed. Switched off: whatever was held goes out now.
    pub fn set_on(&mut self, on: bool) -> Vec<(String, String)> {
        self.on = on;
        if on { Vec::new() } else { std::mem::take(&mut self.held) }
    }

    /// A game started (`true`) or the last one closed (`false`). Returns what to send now.
    pub fn set_playing(&mut self, playing: bool) -> Vec<(String, String)> {
        self.playing = playing;
        if playing { Vec::new() } else { std::mem::take(&mut self.held) }
    }

    /// A notification to show: `Some` to send it now, `None` when it is held for later.
    pub fn submit(&mut self, title: String, body: String) -> Option<(String, String)> {
        if !(self.on && self.playing) {
            return Some((title, body));
        }
        self.held.retain(|(t, _)| *t != title);
        self.held.push((title, body));
        if self.held.len() > HELD_MAX {
            self.held.remove(0);
        }
        None
    }
}

pub static GATE: Mutex<Gate> = Mutex::new(Gate { on: false, playing: false, held: Vec::new() });

pub fn gate() -> std::sync::MutexGuard<'static, Gate> {
    GATE.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(t: &str) -> (String, String) {
        (t.to_string(), String::new())
    }

    #[test]
    fn held_while_playing_and_sent_after() {
        let mut g = Gate::default();
        // Off: everything goes out at once, playing or not.
        g.set_playing(true);
        assert_eq!(g.submit("a".into(), String::new()), Some(n("a")));
        g.set_on(true);
        assert_eq!(g.submit("b".into(), String::new()), None);
        assert_eq!(g.submit("b".into(), String::new()), None, "the same title is held once");
        assert_eq!(g.submit("c".into(), String::new()), None);
        assert_eq!(g.set_playing(false), vec![n("b"), n("c")]);
        // Not playing: straight out.
        assert_eq!(g.submit("d".into(), String::new()), Some(n("d")));
        assert!(g.set_playing(false).is_empty());
    }

    #[test]
    fn switching_it_off_sends_what_was_held_and_the_queue_is_capped() {
        let mut g = Gate::default();
        g.set_on(true);
        g.set_playing(true);
        for i in 0..8 {
            assert!(g.submit(format!("n{i}"), String::new()).is_none());
        }
        let out = g.set_on(false);
        assert_eq!(out.len(), HELD_MAX);
        assert_eq!(out[0].0, "n3");
        assert_eq!(g.submit("x".into(), String::new()), Some(n("x")));
    }
}
