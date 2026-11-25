//! Linux: pads read straight from evdev (`/dev/input/event*`), with no udev library and nothing that needs root.
//! A node the user may not open (no `uaccess` ACL, not in the `input` group) is simply not listed.
//!
//! Only gamepads count (a `BTN_SOUTH` key); the extra nodes a pad brings (DualSense motion sensors, touchpad)
//! and virtual pads (Steam Input's, anything made with uinput) are left out. Buttons follow the kernel's gamepad
//! layout (Documentation/input/gamepad.rst), except the `xpad` driver, which reports X as `BTN_NORTH` and Y as
//! `BTN_WEST` (its `BTN_X`/`BTN_Y`): those two are swapped back. Hot-plug: inotify on `/dev/input`. Nodes are
//! opened only while reading (`set_mode`); otherwise the thread sleeps in `poll` on inotify alone.
//! Battery: the `power_supply` the pad's HID driver registers (hid-playstation, hid-nintendo, xpadneo, ...).
//! Rumble: an `FF_RUMBLE` effect uploaded with `EVIOCSFF` (needs the node open for writing).

use super::{Changes, Connection, PadState, RawPad, Rumble, Source, Waker, btn, clean_name};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEV_DIR: &str = "/dev/input";

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const EV_FF: u16 = 0x15;
const SYN_DROPPED: u16 = 3;
const KEY_MAX: usize = 0x2ff;
const ABS_MAX: usize = 0x3f;
const FF_MAX: usize = 0x7f;
const FF_RUMBLE: u16 = 0x50;

const BTN_SOUTH: u16 = 0x130;
const BTN_EAST: u16 = 0x131;
const BTN_NORTH: u16 = 0x133;
const BTN_WEST: u16 = 0x134;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_TL2: u16 = 0x138;
const BTN_TR2: u16 = 0x139;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_MODE: u16 = 0x13c;
const BTN_THUMBL: u16 = 0x13d;
const BTN_THUMBR: u16 = 0x13e;
const BTN_DPAD_UP: u16 = 0x220;
const BTN_DPAD_DOWN: u16 = 0x221;
const BTN_DPAD_LEFT: u16 = 0x222;
const BTN_DPAD_RIGHT: u16 = 0x223;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;
const AXES: [u16; 8] = [ABS_X, ABS_Y, ABS_Z, ABS_RX, ABS_RY, ABS_RZ, ABS_HAT0X, ABS_HAT0Y];

const BUS_USB: u16 = 0x03;
const BUS_BLUETOOTH: u16 = 0x05;

// _IOC() as on x86, arm and riscv (the flake's systems).
const fn ioc(dir: u64, nr: u64, size: usize) -> u64 {
    (dir << 30) | ((size as u64) << 16) | ((b'E' as u64) << 8) | nr
}
const READ: u64 = 2;
const WRITE: u64 = 1;
const fn eviocgname(len: usize) -> u64 {
    ioc(READ, 0x06, len)
}
const fn eviocguniq(len: usize) -> u64 {
    ioc(READ, 0x08, len)
}
const EVIOCGID: u64 = ioc(READ, 0x02, std::mem::size_of::<libc::input_id>());
const fn eviocgkey(len: usize) -> u64 {
    ioc(READ, 0x18, len)
}
const fn eviocgbit(ev: u16, len: usize) -> u64 {
    ioc(READ, 0x20 + ev as u64, len)
}
const fn eviocgabs(abs: u16) -> u64 {
    ioc(READ, 0x40 + abs as u64, std::mem::size_of::<libc::input_absinfo>())
}
const EVIOCSFF: u64 = ioc(WRITE, 0x80, std::mem::size_of::<libc::ff_effect>());

fn ioctl<T>(fd: RawFd, req: u64, arg: *mut T) -> bool {
    // SAFETY: every request above names the size of what `arg` points to.
    unsafe { libc::ioctl(fd, req as _, arg) >= 0 }
}

fn bit(bits: &[u8], n: usize) -> bool {
    bits.get(n / 8).is_some_and(|b| b & (1 << (n % 8)) != 0)
}

fn open(path: &Path, write: bool) -> Option<OwnedFd> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    let mode = if write { libc::O_RDWR } else { libc::O_RDONLY };
    // SAFETY: a valid C string; the fd is owned from here on.
    let fd = unsafe { libc::open(c.as_ptr(), mode | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    // SAFETY: a fresh fd from open().
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

fn ioctl_string(fd: RawFd, req: impl Fn(usize) -> u64) -> String {
    let mut buf = [0u8; 256];
    if !ioctl(fd, req(buf.len()), buf.as_mut_ptr()) {
        return String::new();
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// The `power_supply` the pad's driver registered: (percent, charging).
pub fn read_battery(sys_dev: &Path) -> Option<(u8, bool)> {
    let dir = std::fs::read_dir(sys_dev.join("power_supply")).ok()?;
    for e in dir.flatten() {
        let capacity = std::fs::read_to_string(e.path().join("capacity")).ok();
        let status = std::fs::read_to_string(e.path().join("status")).unwrap_or_default();
        if let Some(b) = parse_battery(capacity.as_deref(), &status) {
            return Some(b);
        }
    }
    None
}

/// `capacity` (0-100) and `status` ("Charging", "Full", "Discharging", ...) from sysfs.
pub fn parse_battery(capacity: Option<&str>, status: &str) -> Option<(u8, bool)> {
    let pct: u8 = capacity?.trim().parse::<u16>().ok()?.min(100) as u8;
    let charging = matches!(status.trim(), "Charging" | "Full");
    Some((pct, charging))
}

pub fn connection(bustype: u16) -> Option<Connection> {
    match bustype {
        BUS_USB => Some(Connection::Usb),
        BUS_BLUETOOTH => Some(Connection::Bluetooth),
        _ => None,
    }
}

/// -1..1 from an axis range (a centred range; anything else counts from its middle).
pub fn stick(v: i32, min: i32, max: i32) -> f32 {
    if max <= min {
        return 0.0;
    }
    let mid = (min as f64 + max as f64) / 2.0;
    let half = (max as f64 - min as f64) / 2.0;
    (((v as f64 - mid) / half) as f32).clamp(-1.0, 1.0)
}

/// 0..1 from a trigger's range.
pub fn trigger(v: i32, min: i32, max: i32) -> f32 {
    if max <= min {
        return 0.0;
    }
    (((v - min) as f64 / (max - min) as f64) as f32).clamp(0.0, 1.0)
}

/// The kernel key code -> our button bit. `xpad`: the driver that has X and Y the other way round.
pub fn button(code: u16, xpad: bool) -> u32 {
    match code {
        BTN_SOUTH => btn::SOUTH,
        BTN_EAST => btn::EAST,
        BTN_NORTH if xpad => btn::WEST,
        BTN_WEST if xpad => btn::NORTH,
        BTN_NORTH => btn::NORTH,
        BTN_WEST => btn::WEST,
        BTN_TL => btn::LB,
        BTN_TR => btn::RB,
        BTN_TL2 => btn::LT,
        BTN_TR2 => btn::RT,
        BTN_SELECT => btn::SELECT,
        BTN_START => btn::START,
        BTN_MODE => btn::GUIDE,
        BTN_THUMBL => btn::LS,
        BTN_THUMBR => btn::RS,
        BTN_DPAD_UP => btn::UP,
        BTN_DPAD_DOWN => btn::DOWN,
        BTN_DPAD_LEFT => btn::LEFT,
        BTN_DPAD_RIGHT => btn::RIGHT,
        _ => 0,
    }
}

struct Dev {
    node: PathBuf,
    pad: RawPad,
    xpad: bool,
    /// `/sys/class/input/eventN/device/device` (battery lives under it).
    sys: PathBuf,
    fd: Option<OwnedFd>,
    writable: bool,
    ranges: HashMap<u16, (i32, i32)>,
    /// The hat's buttons (D-pad as ABS_HAT0X/Y), kept apart from the key buttons.
    hat: u32,
    keys: u32,
    state: PadState,
    effect: Option<i16>,
}

impl Dev {
    /// Open and check a node: `None` when it is no gamepad, virtual, or not ours to read.
    fn probe(node: &Path) -> Option<Dev> {
        let name = node.file_name()?.to_str()?;
        let class = Path::new("/sys/class/input").join(name);
        let real = std::fs::canonicalize(&class).ok()?;
        if real.to_string_lossy().contains("/devices/virtual/") {
            return None;
        }
        let (fd, writable) = match open(node, true) {
            Some(fd) => (fd, true),
            None => (open(node, false)?, false),
        };
        let raw = fd.as_raw_fd();
        let mut keys = [0u8; KEY_MAX / 8 + 1];
        if !ioctl(raw, eviocgbit(EV_KEY, keys.len()), keys.as_mut_ptr()) || !bit(&keys, BTN_SOUTH as usize) {
            return None;
        }
        let mut id = libc::input_id { bustype: 0, vendor: 0, product: 0, version: 0 };
        ioctl(raw, EVIOCGID, &mut id);
        let mut ff = [0u8; FF_MAX / 8 + 1];
        let rumble = writable && ioctl(raw, eviocgbit(EV_FF, ff.len()), ff.as_mut_ptr()) && bit(&ff, FF_RUMBLE as usize);
        let uniq = ioctl_string(raw, eviocguniq);
        let sys = class.join("device").join("device");
        let driver = std::fs::read_link(sys.join("driver")).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
        let battery = read_battery(&sys);
        let key = if uniq.is_empty() { name.to_string() } else { uniq.to_lowercase() };
        let pad = RawPad {
            id: format!("{:04x}:{:04x}:{key}", id.vendor, id.product),
            name: clean_name(&ioctl_string(raw, eviocgname)),
            vendor: id.vendor,
            connection: connection(id.bustype),
            battery: battery.map(|b| b.0),
            charging: battery.is_some_and(|b| b.1),
            rumble,
        };
        let mut dev = Dev {
            node: node.to_path_buf(),
            pad,
            xpad: driver.as_deref() == Some("xpad"),
            sys,
            fd: Some(fd),
            writable,
            ranges: HashMap::new(),
            hat: 0,
            keys: 0,
            state: PadState::default(),
            effect: None,
        };
        dev.sync();
        Some(dev)
    }

    /// Read the axis ranges and the current state (after opening, or when the kernel dropped events).
    fn sync(&mut self) {
        let Some(fd) = self.fd.as_ref().map(AsRawFd::as_raw_fd) else { return };
        let mut absbits = [0u8; ABS_MAX / 8 + 1];
        ioctl(fd, eviocgbit(EV_ABS, absbits.len()), absbits.as_mut_ptr());
        self.ranges.clear();
        let mut values = Vec::new();
        for a in AXES {
            if !bit(&absbits, a as usize) {
                continue;
            }
            let mut info = libc::input_absinfo { value: 0, minimum: 0, maximum: 0, fuzz: 0, flat: 0, resolution: 0 };
            if ioctl(fd, eviocgabs(a), &mut info) {
                self.ranges.insert(a, (info.minimum, info.maximum));
                values.push((a, info.value));
            }
        }
        let mut keys = [0u8; KEY_MAX / 8 + 1];
        self.keys = 0;
        if ioctl(fd, eviocgkey(keys.len()), keys.as_mut_ptr()) {
            for code in BTN_SOUTH..=BTN_DPAD_RIGHT {
                if bit(&keys, code as usize) {
                    self.keys |= button(code, self.xpad);
                }
            }
        }
        self.hat = 0;
        self.state = PadState::default();
        for (a, v) in values {
            self.abs(a, v);
        }
        self.state.buttons = self.keys | self.hat;
    }

    fn abs(&mut self, code: u16, v: i32) {
        let Some(&(min, max)) = self.ranges.get(&code) else { return };
        let s = &mut self.state;
        match code {
            ABS_X => s.lx = stick(v, min, max),
            ABS_Y => s.ly = stick(v, min, max),
            ABS_RX => s.rx = stick(v, min, max),
            ABS_RY => s.ry = stick(v, min, max),
            ABS_Z => s.lt = trigger(v, min, max),
            ABS_RZ => s.rt = trigger(v, min, max),
            ABS_HAT0X => {
                self.hat &= !(btn::LEFT | btn::RIGHT);
                self.hat |= if v < 0 { btn::LEFT } else if v > 0 { btn::RIGHT } else { 0 };
            }
            ABS_HAT0Y => {
                self.hat &= !(btn::UP | btn::DOWN);
                self.hat |= if v < 0 { btn::UP } else if v > 0 { btn::DOWN } else { 0 };
            }
            _ => {}
        }
    }

    /// Read what is waiting. `None`: the pad is gone.
    fn read(&mut self) -> Option<bool> {
        let fd = self.fd.as_ref()?.as_raw_fd();
        let mut changed = false;
        let mut buf = [libc::input_event { time: libc::timeval { tv_sec: 0, tv_usec: 0 }, type_: 0, code: 0, value: 0 }; 64];
        loop {
            let size = std::mem::size_of_val(&buf);
            // SAFETY: reading plain-old-data events into a buffer of exactly `size` bytes.
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), size) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                return match e.raw_os_error() {
                    Some(libc::EAGAIN) | Some(libc::EINTR) => Some(changed),
                    _ => None,
                };
            }
            if n == 0 {
                return None;
            }
            let count = n as usize / std::mem::size_of::<libc::input_event>();
            for ev in &buf[..count] {
                match (ev.type_, ev.code) {
                    (EV_SYN, SYN_DROPPED) => {
                        self.sync();
                        changed = true;
                    }
                    (EV_KEY, code) => {
                        let b = button(code, self.xpad);
                        if b != 0 {
                            if ev.value != 0 { self.keys |= b } else { self.keys &= !b }
                            changed = true;
                        }
                    }
                    (EV_ABS, code) => {
                        self.abs(code, ev.value);
                        changed = true;
                    }
                    _ => {}
                }
            }
            self.state.buttons = self.keys | self.hat;
            if count < buf.len() {
                return Some(changed);
            }
        }
    }

    fn rumble(&mut self, r: Rumble) -> bool {
        if !self.pad.rumble {
            return false;
        }
        if self.fd.is_none() || !self.writable {
            self.fd = open(&self.node, true);
            self.writable = self.fd.is_some();
            self.effect = None;
        }
        let Some(fd) = self.fd.as_ref().map(AsRawFd::as_raw_fd) else { return false };
        let (strong, weak, ms) = r.params();
        // SAFETY: a zeroed ff_effect is a valid "no effect"; the union's first two u16 are ff_rumble_effect.
        let mut eff: libc::ff_effect = unsafe { std::mem::zeroed() };
        eff.type_ = FF_RUMBLE;
        eff.id = self.effect.unwrap_or(-1);
        eff.replay.length = ms;
        let u = std::ptr::addr_of_mut!(eff.u).cast::<libc::ff_rumble_effect>();
        // SAFETY: the union is at least as large as ff_rumble_effect and suitably aligned.
        unsafe {
            (*u).strong_magnitude = strong;
            (*u).weak_magnitude = weak;
        }
        if !ioctl(fd, EVIOCSFF, &mut eff) {
            self.effect = None;
            return false;
        }
        self.effect = Some(eff.id);
        let play = libc::input_event { time: libc::timeval { tv_sec: 0, tv_usec: 0 }, type_: EV_FF, code: eff.id as u16, value: 1 };
        let size = std::mem::size_of_val(&play);
        // SAFETY: writing one plain-old-data event.
        unsafe { libc::write(fd, std::ptr::addr_of!(play).cast(), size) == size as isize }
    }
}

pub struct Evdev {
    devs: Vec<Dev>,
    inotify: Option<File>,
    wake_r: File,
    wake_w: Arc<OwnedFd>,
    reading: bool,
    /// A node appeared or went: look again at this time (udev sets the permissions a moment after).
    rescan_at: Option<Instant>,
}

impl Evdev {
    pub fn new() -> Option<Evdev> {
        let mut fds = [0; 2];
        // SAFETY: pipe2 fills two fds we then own.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } != 0 {
            return None;
        }
        // SAFETY: fresh fds from pipe2.
        let (wake_r, wake_w) = unsafe { (File::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let inotify = {
            // SAFETY: plain syscalls; the fd is owned by the File.
            let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
            (fd >= 0).then(|| unsafe { File::from_raw_fd(fd) }).filter(|f| {
                let dir = std::ffi::CString::new(DEV_DIR).unwrap();
                unsafe { libc::inotify_add_watch(f.as_raw_fd(), dir.as_ptr(), libc::IN_CREATE | libc::IN_DELETE | libc::IN_ATTRIB) >= 0 }
            })
        };
        let mut me = Evdev { devs: Vec::new(), inotify, wake_r, wake_w: Arc::new(wake_w), reading: false, rescan_at: None };
        me.rescan();
        Some(me)
    }

    /// Look at every event node again (new pads probed, gone ones dropped); `true` if the list changed.
    fn rescan(&mut self) -> bool {
        let mut nodes: Vec<PathBuf> = std::fs::read_dir(DEV_DIR)
            .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("event"))).collect())
            .unwrap_or_default();
        nodes.sort_by_key(|p| p.file_name().and_then(|n| n.to_str()).and_then(|n| n[5..].parse::<u32>().ok()).unwrap_or(u32::MAX));
        let before: Vec<String> = self.devs.iter().map(|d| d.pad.id.clone()).collect();
        self.devs.retain(|d| nodes.contains(&d.node));
        for n in nodes {
            if self.devs.iter().any(|d| d.node == n) {
                continue;
            }
            if let Some(mut d) = Dev::probe(&n) {
                if !self.reading {
                    d.fd = None;
                }
                self.devs.push(d);
            }
        }
        before != self.devs.iter().map(|d| d.pad.id.clone()).collect::<Vec<_>>()
    }
}

impl Source for Evdev {
    fn pads(&mut self) -> Vec<RawPad> {
        for d in &mut self.devs {
            let b = read_battery(&d.sys);
            d.pad.battery = b.map(|b| b.0);
            d.pad.charging = b.is_some_and(|b| b.1);
        }
        self.devs.iter().map(|d| d.pad.clone()).collect()
    }

    fn set_mode(&mut self, read: bool, _watch: bool) {
        if read == self.reading {
            return;
        }
        self.reading = read;
        for d in &mut self.devs {
            if read {
                let (fd, w) = match open(&d.node, true) {
                    Some(fd) => (Some(fd), true),
                    None => (open(&d.node, false), false),
                };
                d.fd = fd;
                d.writable = w;
                d.effect = None;
                d.sync();
            } else {
                d.fd = None;
                d.effect = None;
            }
        }
    }

    fn wait(&mut self, timeout: Duration) -> Changes {
        let mut out = Changes::default();
        let now = Instant::now();
        let timeout = match self.rescan_at {
            Some(at) => timeout.min(at.saturating_duration_since(now)),
            None => timeout,
        };
        let mut pfds = vec![libc::pollfd { fd: self.wake_r.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        if let Some(i) = &self.inotify {
            pfds.push(libc::pollfd { fd: i.as_raw_fd(), events: libc::POLLIN, revents: 0 });
        }
        let first_dev = pfds.len();
        let polled: Vec<usize> = (0..self.devs.len()).filter(|i| self.reading && self.devs[*i].fd.is_some()).collect();
        for &i in &polled {
            pfds.push(libc::pollfd { fd: self.devs[i].fd.as_ref().unwrap().as_raw_fd(), events: libc::POLLIN, revents: 0 });
        }
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: pfds is a valid array of pollfd for the call.
        let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, ms) };
        if n > 0 {
            let mut sink = [0u8; 4096];
            if pfds[0].revents != 0 {
                while matches!((&self.wake_r).read(&mut sink), Ok(k) if k > 0) {}
            }
            if self.inotify.is_some() && pfds[1].revents != 0 {
                if let Some(i) = &mut self.inotify {
                    while matches!(i.read(&mut sink), Ok(k) if k > 0) {}
                }
                self.rescan_at = Some(Instant::now() + Duration::from_millis(300));
            }
            let mut gone = Vec::new();
            for (k, &i) in polled.iter().enumerate() {
                let re = pfds[first_dev + k].revents;
                if re == 0 {
                    continue;
                }
                let d = &mut self.devs[i];
                match (re & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0, d.read()) {
                    (_, Some(true)) => out.states.push((d.pad.id.clone(), d.state)),
                    (false, Some(false)) => {}
                    _ => gone.push(i),
                }
            }
            if !gone.is_empty() {
                for i in gone.into_iter().rev() {
                    self.devs.remove(i);
                }
                out.list = true;
            }
        }
        if self.rescan_at.is_some_and(|at| Instant::now() >= at) {
            self.rescan_at = None;
            out.list |= self.rescan();
        }
        out
    }

    fn rumble(&mut self, id: &str, r: Rumble) -> bool {
        self.devs.iter_mut().find(|d| d.pad.id == id).is_some_and(|d| d.rumble(r))
    }

    fn snapshot(&mut self) -> Vec<(String, PadState)> {
        self.devs.iter().filter(|d| d.fd.is_some()).map(|d| (d.pad.id.clone(), d.state)).collect()
    }

    fn waker(&self) -> Waker {
        let w = self.wake_w.clone();
        Arc::new(move || {
            // SAFETY: writing one byte to our own non-blocking pipe.
            unsafe { libc::write(w.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_and_connection_from_the_system() {
        assert_eq!(parse_battery(Some("72\n"), "Discharging\n"), Some((72, false)));
        assert_eq!(parse_battery(Some("100"), "Full"), Some((100, true)));
        assert_eq!(parse_battery(Some("55"), "Charging"), Some((55, true)));
        assert_eq!(parse_battery(None, "Charging"), None, "no capacity: unknown, not 0");
        assert_eq!(parse_battery(Some("n/a"), ""), None);
        assert_eq!(connection(0x03), Some(Connection::Usb));
        assert_eq!(connection(0x05), Some(Connection::Bluetooth));
        assert_eq!(connection(0x06), None);
    }

    #[test]
    fn battery_is_read_from_the_drivers_power_supply() {
        let dir = std::env::temp_dir().join(format!("grapple-pads-{}", std::process::id()));
        let ps = dir.join("power_supply/ps-controller-battery-aa:bb");
        std::fs::create_dir_all(&ps).unwrap();
        std::fs::write(ps.join("capacity"), "40\n").unwrap();
        std::fs::write(ps.join("status"), "Discharging\n").unwrap();
        assert_eq!(read_battery(&dir), Some((40, false)));
        assert_eq!(read_battery(&dir.join("nothing")), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn axes_are_scaled() {
        assert!((stick(128, 0, 255) - 0.0039).abs() < 0.001, "the middle of 0..255 is about 0");
        assert_eq!(stick(0, 0, 255), -1.0);
        assert_eq!(stick(-32768, -32768, 32767), -1.0);
        assert_eq!(stick(32767, -32768, 32767), 1.0);
        assert_eq!(trigger(255, 0, 255), 1.0);
        assert_eq!(trigger(0, 0, 1023), 0.0);
        assert_eq!(stick(5, 3, 3), 0.0, "an empty range is ignored");
    }

    #[test]
    fn xpad_has_x_and_y_the_other_way_round() {
        assert_eq!(button(BTN_NORTH, false), btn::NORTH);
        assert_eq!(button(BTN_NORTH, true), btn::WEST);
        assert_eq!(button(BTN_WEST, true), btn::NORTH);
        assert_eq!(button(BTN_SOUTH, true), btn::SOUTH);
        assert_eq!(button(BTN_MODE, false), btn::GUIDE);
        assert_eq!(button(0x110, false), 0, "a mouse button is nothing");
    }

    #[test]
    fn scanning_this_machine_lists_only_pads_and_never_fails() {
        // Whatever is plugged in (often nothing): no panic, and nothing that is not a gamepad.
        let Some(mut e) = Evdev::new() else { return };
        for p in e.pads() {
            assert!(!p.id.is_empty() && !p.name.is_empty());
        }
        e.set_mode(true, true);
        let _ = e.wait(Duration::from_millis(10));
        e.set_mode(false, true);
        (e.waker())();
        let t = Instant::now();
        let _ = e.wait(Duration::from_secs(5));
        assert!(t.elapsed() < Duration::from_secs(2), "the waker ends the wait");
    }

    #[test]
    fn ioctl_numbers_match_the_kernel() {
        // From linux/input.h on x86_64.
        assert_eq!(EVIOCGID, 0x8008_4502);
        assert_eq!(eviocgname(256), 0x8100_4506);
        assert_eq!(eviocgbit(EV_KEY, 96), 0x8060_4521);
        assert_eq!(eviocgabs(ABS_X), 0x8018_4540);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(EVIOCSFF, 0x4030_4580);
    }
}
