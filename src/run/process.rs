//! Child processes: process groups, cancellation, and handing the terminal to `interactive`
//! programs.

use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{SigSet, Signal, kill, killpg};
use nix::sys::termios::{SetArg, Termios, tcgetattr, tcsetattr};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{Pid, getpgrp, setpgid, tcsetpgrp};
use signal_hook::SigId;
use signal_hook::consts::{SIGCHLD, SIGHUP, SIGINT, SIGTERM, SIGTSTP};

/// How long korin keeps reading pipes after the child is gone.
const DRAIN: Duration = Duration::from_secs(2);
const TICK: Duration = Duration::from_millis(100);

/// The signals korin reacts to while it runs. Each signal writes to its own pipe, so the
/// runner can wait on output and signals together.
pub struct Signals {
    interrupt: UnixStream,
    terminate: UnixStream,
    child: UnixStream,
    ids: Vec<SigId>,
}

impl Signals {
    /// Installs the handlers. While they are installed, Ctrl-C does not kill korin; the runner
    /// sees it and cancels cleanly. `SIGTSTP` is caught and ignored, so korin itself cannot be
    /// suspended. Caught signals reset to their defaults in children on exec.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if a pipe or handler cannot be created.
    pub fn install() -> io::Result<Self> {
        let mut ids = Vec::new();
        let mut pipe = |signals: &[i32]| -> io::Result<UnixStream> {
            let (read, write) = UnixStream::pair()?;
            read.set_nonblocking(true)?;
            write.set_nonblocking(true)?;
            for signal in signals {
                ids.push(signal_hook::low_level::pipe::register(
                    *signal,
                    write.try_clone()?,
                )?);
            }
            Ok(read)
        };
        let interrupt = pipe(&[SIGINT])?;
        let terminate = pipe(&[SIGTERM, SIGHUP])?;
        let child = pipe(&[SIGCHLD])?;
        ids.push(signal_hook::flag::register(
            SIGTSTP,
            Arc::new(AtomicBool::new(false)),
        )?);
        Ok(Self {
            interrupt,
            terminate,
            child,
            ids,
        })
    }

    fn take(stream: &UnixStream) -> bool {
        let mut buf = [0u8; 64];
        let mut any = false;
        let mut reader = stream;
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            any = true;
        }
        any
    }

    pub fn take_interrupt(&self) -> bool {
        Self::take(&self.interrupt)
    }

    pub fn take_terminate(&self) -> bool {
        Self::take(&self.terminate)
    }

    fn take_child(&self) -> bool {
        Self::take(&self.child)
    }

    pub(crate) fn fds(&self) -> [BorrowedFd<'_>; 3] {
        [
            self.interrupt.as_fd(),
            self.terminate.as_fd(),
            self.child.as_fd(),
        ]
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.ids.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

/// Why korin ended a child itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancel {
    Interrupt,
    Timeout,
    Terminate,
    Killed,
}

impl Cancel {
    pub fn describe(self) -> &'static str {
        match self {
            Cancel::Interrupt => "interrupted with Ctrl-C",
            Cancel::Timeout => "timed out",
            Cancel::Terminate => "korin received SIGTERM or SIGHUP",
            Cancel::Killed => "killed at the suspend prompt",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Exited(i32),
    Signaled(Signal),
}

#[derive(Debug)]
pub struct Finished {
    pub status: Status,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub cancelled: Option<Cancel>,
    /// Pipes stayed open after the child was gone: a descendant left the process group.
    pub escaped: bool,
}

pub struct Spec<'a> {
    pub exe: &'a Path,
    pub arg0: &'a str,
    pub args: &'a [String],
    pub env: &'a [(String, String)],
    pub cwd: &'a Path,
    pub timeout: Option<Duration>,
    pub grace: Duration,
}

/// The environment a child gets: korin's own, minus every `KORIN_SECRET_*` variable, plus the
/// step's `env` lines. A secret reaches a child only through an `env` line that names it.
fn child_env(extra: &[(String, String)]) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os()
        .filter(|(key, _)| !key.to_string_lossy().starts_with("KORIN_SECRET_"))
        .collect();
    env.extend(extra.iter().map(|(k, v)| (k.into(), v.into())));
    env
}

fn status_of(status: std::process::ExitStatus) -> Status {
    match (status.code(), status.signal()) {
        (Some(code), _) => Status::Exited(code),
        (None, Some(signal)) => {
            Signal::try_from(signal).map_or(Status::Exited(128 + signal), Status::Signaled)
        }
        (None, None) => Status::Exited(-1),
    }
}

fn read_some<R: Read>(pipe: &mut Option<R>, buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let reader = pipe.as_mut()?;
    let mut chunk = vec![0u8; 65536];
    match reader.read(&mut chunk) {
        Ok(0) | Err(_) => {
            *pipe = None;
            None
        }
        Ok(n) => {
            buf.extend_from_slice(&chunk[..n]);
            Some(chunk[..n].to_vec())
        }
    }
}

struct Cancelling {
    reason: Cancel,
    grace_deadline: Instant,
    killed_at: Option<Instant>,
}

/// Runs a command in its own process group and collects its output.
///
/// `on_output` receives each chunk as it arrives (`true` for stderr). Cancellation follows the
/// protocol in `docs/language.md`: `SIGTERM` to the group, a grace period, `SIGKILL` to the
/// group, reap, then at most two more seconds of draining.
///
/// # Errors
///
/// Returns an I/O error if the command cannot start.
pub fn execute(
    spec: &Spec<'_>,
    signals: &Signals,
    on_output: &mut dyn FnMut(bool, &[u8]),
) -> io::Result<Finished> {
    let mut child: Child = Command::new(spec.exe)
        .arg0(spec.arg0)
        .args(spec.args)
        .env_clear()
        .envs(child_env(spec.env))
        .current_dir(spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let pgid = Pid::from_raw(i32::try_from(child.id()).unwrap_or(i32::MAX));
    let mut out: Option<ChildStdout> = child.stdout.take();
    let mut err: Option<ChildStderr> = child.stderr.take();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let deadline = spec.timeout.map(|t| Instant::now() + t);
    let mut cancelling: Option<Cancelling> = None;
    let mut status: Option<Status> = None;
    let mut reaped_at: Option<Instant> = None;
    let mut escaped = false;

    loop {
        let now = Instant::now();
        // Start or escalate cancellation.
        let interrupted = signals.take_interrupt();
        let terminated = signals.take_terminate();
        match &mut cancelling {
            None => {
                let reason = if terminated {
                    Some(Cancel::Terminate)
                } else if interrupted {
                    Some(Cancel::Interrupt)
                } else if deadline.is_some_and(|d| now >= d) && status.is_none() {
                    Some(Cancel::Timeout)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    let _ = killpg(pgid, Signal::SIGTERM);
                    cancelling = Some(Cancelling {
                        reason,
                        grace_deadline: now + spec.grace,
                        killed_at: None,
                    });
                }
            }
            Some(c) => {
                if c.killed_at.is_none() && (interrupted || terminated || now >= c.grace_deadline) {
                    let _ = killpg(pgid, Signal::SIGKILL);
                    c.killed_at = Some(now);
                }
            }
        }

        if status.is_none()
            && let Ok(Some(exit)) = child.try_wait()
        {
            status = Some(status_of(exit));
            reaped_at = Some(now);
        }

        if status.is_some() && out.is_none() && err.is_none() {
            // The leader is gone, but a descendant that ignored SIGTERM may still run with its
            // output redirected elsewhere. A cancellation always ends with SIGKILL to the group.
            if let Some(Cancelling {
                killed_at: None, ..
            }) = &cancelling
            {
                let _ = killpg(pgid, Signal::SIGKILL);
            }
            break;
        }
        if let Some(reaped) = reaped_at {
            let drain_from = match &cancelling {
                Some(Cancelling {
                    killed_at: None, ..
                }) => None,
                Some(Cancelling {
                    killed_at: Some(killed),
                    ..
                }) => Some((*killed).max(reaped)),
                None => Some(reaped),
            };
            if drain_from.is_some_and(|from| now >= from + DRAIN) {
                escaped = out.is_some() || err.is_some();
                break;
            }
        }

        // Wake at the next deadline, but never spin: a passed deadline waits a full tick.
        let mut wait = TICK;
        let next = match &cancelling {
            Some(Cancelling {
                killed_at: None,
                grace_deadline,
                ..
            }) => Some(*grace_deadline),
            Some(_) => None,
            None => deadline,
        };
        if let Some(next) = next {
            let left = next.saturating_duration_since(now);
            if !left.is_zero() {
                wait = wait.min(left);
            }
        }
        let mut fds: Vec<PollFd<'_>> = Vec::with_capacity(5);
        let [sig_int, sig_term, sig_child] = signals.fds();
        for fd in [sig_int, sig_term, sig_child] {
            fds.push(PollFd::new(fd, PollFlags::POLLIN));
        }
        let out_fd = out.as_ref().map(AsRawFd::as_raw_fd);
        let err_fd = err.as_ref().map(AsRawFd::as_raw_fd);
        let out_borrow = out.as_ref().map(AsFd::as_fd);
        let err_borrow = err.as_ref().map(AsFd::as_fd);
        if let Some(fd) = out_borrow {
            fds.push(PollFd::new(fd, PollFlags::POLLIN));
        }
        if let Some(fd) = err_borrow {
            fds.push(PollFd::new(fd, PollFlags::POLLIN));
        }
        let millis = u16::try_from(wait.as_millis()).unwrap_or(u16::MAX);
        let _ = poll(&mut fds, PollTimeout::from(millis));
        let ready: Vec<i32> = fds
            .iter()
            .filter(|fd| fd.revents().is_some_and(|r| !r.is_empty()))
            .map(|fd| fd.as_fd().as_raw_fd())
            .collect();
        drop(fds);
        signals.take_child();
        if out_fd.is_some_and(|fd| ready.contains(&fd))
            && let Some(chunk) = read_some(&mut out, &mut stdout)
        {
            on_output(false, &chunk);
        }
        if err_fd.is_some_and(|fd| ready.contains(&fd))
            && let Some(chunk) = read_some(&mut err, &mut stderr)
        {
            on_output(true, &chunk);
        }
    }

    Ok(Finished {
        status: status.unwrap_or(Status::Exited(-1)),
        stdout,
        stderr,
        cancelled: cancelling.map(|c| c.reason),
        escaped,
    })
}

/// What to do with an `interactive` program that the human suspended with Ctrl-Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suspended {
    Resume,
    Kill,
}

/// Restores the terminal on every exit path, including a panic.
struct TerminalGuard {
    saved: Termios,
    own: Pid,
    child: Option<Pid>,
}

impl TerminalGuard {
    fn reclaim(&self) {
        give_terminal(self.own);
        let _ = tcsetattr(std::io::stdin(), SetArg::TCSADRAIN, &self.saved);
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            let _ = killpg(child, Signal::SIGKILL);
            let _ = waitpid(child, None);
        }
        self.reclaim();
    }
}

/// Makes `pgid` the terminal's foreground group. `SIGTTOU` is blocked during the call, so korin
/// can take the terminal back while it is in the background without being stopped.
fn give_terminal(pgid: Pid) {
    let mut ttou = SigSet::empty();
    ttou.add(Signal::SIGTTOU);
    let _ = ttou.thread_block();
    let _ = tcsetpgrp(std::io::stdin(), pgid);
    let _ = ttou.thread_unblock();
}

/// Ends a process group: `SIGCONT` in case it is stopped, `SIGTERM`, the grace period, then
/// `SIGKILL`, and reaps the leader.
fn end_group(pid: Pid, grace: Duration, signals: &Signals) -> Status {
    let _ = killpg(pid, Signal::SIGCONT);
    let _ = killpg(pid, Signal::SIGTERM);
    let deadline = Instant::now() + grace;
    loop {
        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            // The leader is gone; SIGKILL whatever else is left in its group.
            Ok(WaitStatus::Exited(_, code)) => {
                let _ = killpg(pid, Signal::SIGKILL);
                return Status::Exited(code);
            }
            Ok(WaitStatus::Signaled(_, signal, _)) => {
                let _ = killpg(pid, Signal::SIGKILL);
                return Status::Signaled(signal);
            }
            Err(_) => {
                let _ = killpg(pid, Signal::SIGKILL);
                return Status::Exited(-1);
            }
            _ => {}
        }
        if Instant::now() >= deadline || signals.take_interrupt() {
            let _ = killpg(pid, Signal::SIGKILL);
            return match waitpid(pid, None) {
                Ok(WaitStatus::Signaled(_, signal, _)) => Status::Signaled(signal),
                Ok(WaitStatus::Exited(_, code)) => Status::Exited(code),
                _ => Status::Signaled(Signal::SIGKILL),
            };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub struct Handoff {
    pub status: Status,
    pub cancelled: Option<Cancel>,
}

/// Gives the terminal to an `interactive` program and waits for it, following the shell
/// job-control protocol. `on_suspend` asks the human what to do when the program is stopped
/// with Ctrl-Z; korin holds the terminal while it asks.
///
/// # Errors
///
/// Returns an I/O error if the terminal modes cannot be read or the program cannot start.
pub fn handoff(
    spec: &Spec<'_>,
    signals: &Signals,
    on_suspend: &mut dyn FnMut() -> Suspended,
) -> io::Result<Handoff> {
    let Spec {
        exe,
        arg0,
        args,
        env,
        cwd,
        grace,
        ..
    } = *spec;
    let saved = tcgetattr(std::io::stdin())?;
    let own = getpgrp();
    let child = Command::new(exe)
        .arg0(arg0)
        .args(args)
        .env_clear()
        .envs(child_env(env))
        .current_dir(cwd)
        .process_group(0)
        .spawn()?;
    let pid = Pid::from_raw(i32::try_from(child.id()).unwrap_or(i32::MAX));
    let _ = setpgid(pid, pid);
    let mut guard = TerminalGuard {
        saved,
        own,
        child: Some(pid),
    };
    give_terminal(pid);

    let finish = |guard: &mut TerminalGuard, status: Status, cancelled: Option<Cancel>| {
        guard.child = None;
        guard.reclaim();
        Handoff { status, cancelled }
    };

    loop {
        if signals.take_terminate() {
            let status = end_group(pid, grace, signals);
            return Ok(finish(&mut guard, status, Some(Cancel::Terminate)));
        }
        match waitpid(pid, Some(WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED)) {
            Ok(WaitStatus::Exited(_, code)) => {
                return Ok(finish(&mut guard, Status::Exited(code), None));
            }
            Ok(WaitStatus::Signaled(_, signal, _)) => {
                return Ok(finish(&mut guard, Status::Signaled(signal), None));
            }
            Ok(WaitStatus::Stopped(_, Signal::SIGTTIN | Signal::SIGTTOU)) => {
                // The program touched the terminal before korin handed it over.
                give_terminal(pid);
                let _ = kill(pid, Signal::SIGCONT);
                let _ = killpg(pid, Signal::SIGCONT);
            }
            Ok(WaitStatus::Stopped(_, _)) => {
                let child_modes = tcgetattr(std::io::stdin()).ok();
                guard.reclaim();
                match on_suspend() {
                    Suspended::Resume => {
                        if let Some(modes) = &child_modes {
                            let _ = tcsetattr(std::io::stdin(), SetArg::TCSADRAIN, modes);
                        }
                        give_terminal(pid);
                        let _ = killpg(pid, Signal::SIGCONT);
                    }
                    Suspended::Kill => {
                        let status = end_group(pid, grace, signals);
                        return Ok(finish(&mut guard, status, Some(Cancel::Killed)));
                    }
                }
            }
            Err(error) => return Err(io::Error::other(error)),
            Ok(_) => {}
        }
        let [_, sig_term, sig_child] = signals.fds();
        let mut fds = [
            PollFd::new(sig_term, PollFlags::POLLIN),
            PollFd::new(sig_child, PollFlags::POLLIN),
        ];
        let millis = u16::try_from(TICK.as_millis()).unwrap_or(u16::MAX);
        let _ = poll(&mut fds, PollTimeout::from(millis));
        signals.take_child();
    }
}
