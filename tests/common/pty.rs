//! A pty session for tests that need a real terminal. The child runs in a new session with
//! the pty as its controlling terminal.

#![allow(dead_code)] // Each test crate uses a different part of this module.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

pub struct Session {
    output: Arc<Mutex<Vec<u8>>>,
    writer: Box<dyn Write + Send>,
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
    pub master: Box<dyn portable_pty::MasterPty + Send>,
}

impl Session {
    /// Starts `cmd` on a new pty of the given size, and collects everything it writes.
    pub fn spawn(cmd: CommandBuilder, rows: u16, cols: u16) -> Self {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let child = pty.slave.spawn_command(cmd).unwrap();
        let mut reader = pty.master.try_clone_reader().unwrap();
        let writer = pty.master.take_writer().unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&output);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        Self {
            output,
            writer,
            child,
            master: pty.master,
        }
    }

    pub fn wait_for(&self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.text().contains(text) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for {text:?}; output:\n{}", self.text());
    }

    /// Waits until the child has written nothing for `quiet`, or has exited. Returns true if
    /// it exited.
    pub fn settle(&mut self, quiet: Duration) -> bool {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut seen = self.bytes().len();
        let mut since = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(25));
            if self.child.try_wait().unwrap().is_some() {
                return true;
            }
            let now = self.bytes().len();
            if now != seen {
                seen = now;
                since = Instant::now();
            } else if since.elapsed() >= quiet {
                return false;
            }
            assert!(
                Instant::now() < deadline,
                "korin never went quiet; output:\n{}",
                self.text()
            );
        }
    }

    pub fn send(&mut self, text: &str) {
        self.writer.write_all(text.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    pub fn wait(&mut self) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                // Let the reader thread catch up with the last output.
                std::thread::sleep(Duration::from_millis(100));
                return status.exit_code();
            }
            assert!(
                Instant::now() < deadline,
                "korin did not exit; output:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// What the terminal shows: the output without color codes.
    pub fn text(&self) -> String {
        visible(&self.raw())
    }

    /// The output as korin wrote it, color codes included.
    pub fn raw(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }

    /// The exact bytes korin wrote.
    pub fn bytes(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }

    pub fn pid(&self) -> i32 {
        i32::try_from(self.child.process_id().unwrap()).unwrap()
    }
}

/// Removes color and cursor codes, so tests match the text korin wrote.
pub fn visible(text: &str) -> String {
    regex::Regex::new("\x1b\\[[0-9;?]*[A-Za-z]")
        .unwrap()
        .replace_all(text, "")
        .into_owned()
}
