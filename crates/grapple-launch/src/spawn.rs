//! Starting processes the way the desktop starts an app.
//!
//! * Environment: the session environment Grapple started with ([`crate::env::for_child`]) plus the plan's own
//!   overrides. Nothing Grapple set for its window (`WEBKIT_*`, `__NV_*`) reaches a game or Steam.
//! * Cgroup: each child gets its own transient systemd user scope (`systemd-run --user --scope`, as uwsm/the app
//!   launcher do), so it is not part of Grapple's scope: systemd-oomd or stopping Grapple's scope never takes a
//!   game or Steam with it. Where the user manager is not reachable, it falls back to just `setsid`.
//! * Session: always its own (`setsid`), so quitting Grapple or closing its terminal never kills a game or Steam.
//! * [`spawn_outside`] (Steam) also leaves Grapple's process tree: a double fork hands it to the user manager
//!   (the subreaper), like Steam started from the desktop.
//! * Windows: no scopes or sessions. Children are `DETACHED_PROCESS` (no console window) in their own process group
//!   (`CREATE_NEW_PROCESS_GROUP`, so a Ctrl+C or console close of Grapple never reaches them) and break away from a
//!   job object Grapple may run in (`CREATE_BREAKAWAY_FROM_JOB`, where the job allows it), so stopping Grapple never
//!   takes them along. Windows does not keep exited children as zombies, so nothing needs reaping.

use crate::error::{LaunchError, Result};
use crate::plan::{LaunchPlan, Secret};
use crate::proc::ProcFs;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::watch;

pub struct Spawned {
    pub pid: u32,
    /// Start time of the process in clock ticks (to tell it apart from a reused pid).
    pub starttime: u64,
    /// `Some(code)` once the process has exited (`-1` if it died from a signal).
    pub exit: watch::Receiver<Option<i32>>,
}

static SCOPES: OnceLock<bool> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Can children get their own systemd user scope? Checked once (a `systemd-run --user --scope true`, ~30 ms).
/// `GRAPPLE_NO_SCOPE=1` turns scopes off. Call early from a blocking context to keep the check off a launch.
pub fn scopes_available() -> bool {
    *SCOPES.get_or_init(|| {
        if cfg!(windows) || std::env::var_os("GRAPPLE_NO_SCOPE").is_some() {
            return false;
        }
        let path = crate::env::child_var("PATH").unwrap_or_default();
        let Some(bin) = crate::paths::which_in("systemd-run", &path.to_string_lossy()) else { return false };
        std::process::Command::new(bin)
            .args(["--user", "--scope", "--collect", "--quiet", "--", "true"])
            .env_clear()
            .envs(crate::env::for_child())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// The slice children go into: the one Grapple itself runs in when that is an app slice (uwsm's
/// `app-graphical.slice` when Grapple was started from the desktop or a terminal), else `app.slice`.
fn slice() -> &'static str {
    static SLICE: OnceLock<String> = OnceLock::new();
    SLICE.get_or_init(|| {
        let cg = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
        cg.lines()
            .find_map(|l| l.strip_prefix("0::"))
            .and_then(|path| path.split('/').rev().find(|c| c.ends_with(".slice") && c.starts_with("app")).map(str::to_string))
            .unwrap_or_else(|| "app.slice".to_string())
    })
}

/// `program args` wrapped in `systemd-run --user --scope` when scopes work. The scope runs the program in place
/// (same pid, `exec`), so the pid Grapple tracks is the game's.
fn scoped(program: &str, args: &[String]) -> (String, Vec<String>) {
    if !scopes_available() {
        return (program.to_string(), args.to_vec());
    }
    let base = Path::new(program).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name: String = base.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(32).collect();
    let unit = format!("app-grapple-{name}-{}-{}.scope", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed));
    let mut a = vec![
        "--user".to_string(),
        "--scope".into(),
        "--collect".into(),
        "--quiet".into(),
        format!("--slice={}", slice()),
        format!("--unit={unit}"),
        // Without a description systemd names the scope after the whole command line and writes it to the
        // journal ("Started [systemd-run] umu-run ... -AUTH_PASSWORD=<exchange code> ...").
        format!("--description=grapple: {base}"),
        "--".into(),
        program.to_string(),
    ];
    a.extend(args.iter().cloned());
    ("systemd-run".to_string(), a)
}

fn command(program: &str, args: &[String], env: &BTreeMap<String, String>, cwd: Option<&Path>) -> std::process::Command {
    let (program, args) = scoped(program, args);
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    cmd.env_clear();
    cmd.envs(crate::env::for_child());
    for (k, v) in env {
        cmd.env(k, v);
    }
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd.stdin(Stdio::null());
    cmd
}

/// Spawn `program args` detached (own session, own scope, clean environment plus `env`). `log`, if given,
/// receives stdout and stderr (truncated first).
pub fn spawn_detached(
    program: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: Option<&Path>,
    log: Option<&Path>,
    fs: &ProcFs,
) -> Result<Spawned> {
    let mut cmd = command(program, args, env, cwd);
    match log {
        Some(path) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let out = std::fs::File::create(path)?;
            let err = out.try_clone()?;
            cmd.stdout(Stdio::from(out)).stderr(Stdio::from(err));
        }
        None => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid(2) is async-signal-safe and touches no memory; it runs in the forked child before exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut cmd = tokio::process::Command::from(cmd);
    cmd.kill_on_drop(false);
    #[cfg(not(windows))]
    let spawned = cmd.spawn();
    #[cfg(windows)]
    let spawned = detached(|flags| {
        cmd.creation_flags(flags);
        cmd.spawn()
    });
    let mut child = spawned.map_err(|source| LaunchError::Spawn { program: program.to_string(), source })?;
    let pid = child.id().ok_or_else(|| LaunchError::Config("process exited before it could be tracked".into()))?;
    let starttime = fs.stat(pid).map(|s| s.3).unwrap_or(0);
    let (tx, rx) = watch::channel(None);
    // Reap the child (no zombies) and publish its exit code.
    tokio::spawn(async move {
        let code = match child.wait().await {
            Ok(status) => status.code().unwrap_or(-1),
            Err(_) => -1,
        };
        let _ = tx.send(Some(code));
    });
    Ok(Spawned { pid, starttime, exit: rx })
}

/// Spawn with the Windows creation flags of this module (see the module docs). A job object that forbids breaking
/// away makes CreateProcess fail with "access denied": then the child stays in the job.
#[cfg(windows)]
fn detached<T>(mut spawn: impl FnMut(u32) -> std::io::Result<T>) -> std::io::Result<T> {
    use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
    const ACCESS_DENIED: i32 = 5;
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    match spawn(base | CREATE_BREAKAWAY_FROM_JOB) {
        Err(e) if e.raw_os_error() == Some(ACCESS_DENIED) => spawn(base),
        r => r,
    }
}

/// Start `program args` outside Grapple on Windows: clean environment, detached, own process group, not waited for
/// (Steam: `steam.exe -applaunch` hands its request to the running client and exits).
#[cfg(windows)]
pub fn spawn_outside(program: &str, args: &[String]) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let mut cmd = command(program, args, &BTreeMap::new(), None);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    detached(|flags| {
        cmd.creation_flags(flags);
        cmd.spawn()
    })
    .map_err(|source| LaunchError::Spawn { program: program.to_string(), source })?;
    Ok(())
}

/// Start `program args` completely outside Grapple: clean environment, own session, own scope, and not a child
/// of Grapple (double fork; the user manager adopts it). Returns once the program has been exec'd. Used for
/// Steam, which must behave exactly like a Steam started from the desktop.
#[cfg(not(windows))]
pub fn spawn_outside(program: &str, args: &[String]) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let mut cmd = command(program, args, &BTreeMap::new(), None);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    // SAFETY: only async-signal-safe calls (setsid, fork, _exit) between fork and exec. The intermediate process
    // exits at once; the grandchild goes on to exec. std reports an exec failure of the grandchild through its
    // CLOEXEC pipe, which the intermediate's exit does not close for the grandchild.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            match libc::fork() {
                -1 => Err(std::io::Error::last_os_error()),
                0 => Ok(()),
                _ => libc::_exit(0),
            }
        });
    }
    let mut child = cmd.spawn().map_err(|source| LaunchError::Spawn { program: program.to_string(), source })?;
    // The intermediate process exits right after its fork; reap it.
    let _ = child.wait();
    Ok(())
}

/// Spawn a [`LaunchPlan`], filling in the exchange code. This is the only place a real code meets a command line.
pub fn spawn_plan(plan: &LaunchPlan, code: Option<&Secret>, log: Option<&Path>, fs: &ProcFs) -> Result<Spawned> {
    let args = plan.resolve_args(code);
    spawn_detached(&plan.program, &args, &plan.env, plan.cwd.as_deref(), log, fs)
}
