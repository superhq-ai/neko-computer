use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The cat that sits on stderr while the CLI waits: mostly it just sits
/// there, sometimes it blinks. The same cat reaches out in the tunnel-open
/// line, `(=^..^=)つ`, so the poses read as one character. Frames share one
/// width so the line never shifts. Skipped entirely when stderr is not a
/// terminal, and erased before any real output prints.
const FRAMES: &[&str] = &["(=^..^=)", "(=^..^=)", "(=^..^=)", "(=^..^=)", "(=^--^=)"];

#[derive(Clone)]
pub struct Mascot {
    stop: Arc<AtomicBool>,
    active: bool,
}

impl Mascot {
    pub fn start(label: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let active = std::io::stderr().is_terminal();
        if active {
            let flag = stop.clone();
            let label = label.to_string();
            tokio::spawn(async move {
                let mut tick = 0usize;
                loop {
                    if flag.load(Ordering::Relaxed) {
                        break;
                    }
                    let face = FRAMES[tick % FRAMES.len()];
                    eprint!("\r\x1b[2K{face}  {label}");
                    let _ = std::io::stderr().flush();
                    tick += 1;
                    tokio::time::sleep(Duration::from_millis(240)).await;
                }
            });
        }
        Self { stop, active }
    }

    /// Stop the animation and erase its line. Idempotent, callable from any
    /// clone, and synchronous so it works inside the tunnel ready callback.
    pub fn finish(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if self.active {
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
        }
    }
}
