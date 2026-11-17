//! Trace output for `--probe --verbose`; silent in the tray app.

use std::sync::atomic::{AtomicBool, Ordering};

pub static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

#[macro_export]
macro_rules! vlog {
    ($($arg:tt)*) => {
        if $crate::diag::verbose() {
            println!("    · {}", format!($($arg)*));
        }
    };
}
