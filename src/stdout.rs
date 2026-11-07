//! `println!` and `print!` that never panic on a closed stdout.
//!
//! Rust ignores SIGPIPE, so the standard macros panic when the reader has
//! gone (`niructl keys | head`). These shadow them for the whole crate: they
//! are declared first in each crate root, which puts them ahead of the std
//! prelude.
//!
//! A command-line tool calls [`exit_on_broken_pipe`] and then ends quietly
//! (status 0) once nobody reads its output, like a tool that keeps the
//! default SIGPIPE action. A long-running program such as NIRUWM leaves it
//! off and never stops because of a log pipe: the output is dropped.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

static EXIT_ON_BROKEN_PIPE: AtomicBool = AtomicBool::new(false);

/// End the process quietly when stdout's reader has gone.
#[allow(dead_code)] // only the command-line binaries opt in
pub fn exit_on_broken_pipe() {
    EXIT_ON_BROKEN_PIPE.store(true, Ordering::Relaxed);
}

/// Write `args` (and a newline) to stdout; see the module documentation.
pub fn write(args: std::fmt::Arguments<'_>, newline: bool) {
    let mut out = std::io::stdout().lock();
    let mut result = out.write_fmt(args);
    if newline {
        result = result.and_then(|()| out.write_all(b"\n"));
    }
    if let Err(e) = result {
        if e.kind() == std::io::ErrorKind::BrokenPipe && EXIT_ON_BROKEN_PIPE.load(Ordering::Relaxed)
        {
            std::process::exit(0);
        }
    }
}

#[allow(unused_macros)]
macro_rules! println {
    () => {
        $crate::stdout::write(format_args!(""), true)
    };
    ($($arg:tt)*) => {
        $crate::stdout::write(format_args!($($arg)*), true)
    };
}

#[allow(unused_macros)]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::stdout::write(format_args!($($arg)*), false)
    };
}
