use std::{
    fs::File,
    io,
    path::Path,
    sync::{Mutex, OnceLock, atomic::AtomicBool},
};

pub const HOST_PREFIX: &str = "macOS";
pub const GUEST_PREFIX: &str = "Linux";

pub static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();
pub static CONSOLE_LOG_ENABLED: AtomicBool = AtomicBool::new(true);

pub fn init_log_file(path: impl AsRef<Path>) -> io::Result<()> {
    let log_file = File::create(&path)?;
    LOG_FILE.get_or_init(|| Mutex::new(log_file));
    Ok(())
}

pub fn disable_console_log() {
    CONSOLE_LOG_ENABLED.store(false, std::sync::atomic::Ordering::Relaxed);
}

pub enum Prefix {
    Host,
    Guest,
}

#[macro_export]
macro_rules! println_impl {
    ($print_macro:ident, $prefix:ident, $fmt:expr, $($args:tt)*) => {{
        if $crate::log::CONSOLE_LOG_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
            $print_macro!(concat!("{}: ", $fmt), $crate::log::$prefix, $($args)*);
        }
        if let Some(log_file) = $crate::log::LOG_FILE.get() {
            use std::io::Write;
            let mut log_file = log_file.lock().unwrap();
            writeln!(&mut log_file, concat!("{}: ", $fmt), $crate::log::$prefix, $($args)*).unwrap();
        }
    }};
    ($print_macro:ident, $prefix:ident, $fmt:expr) => {
        $crate::println_impl!($print_macro, $prefix, $fmt, );
    };
}

#[macro_export]
macro_rules! host_println {
    ($($arg:tt)*) => {
        $crate::println_impl!(println, HOST_PREFIX, $($arg)*);
    };
}

#[macro_export]
macro_rules! host_eprintln {
    ($($arg:tt)*) => {
        $crate::println_impl!(eprintln, HOST_PREFIX, $($arg)*);
    };
}

#[macro_export]
macro_rules! guest_println {
    ($($arg:tt)*) => {
        $crate::println_impl!(println, GUEST_PREFIX, $($arg)*);
    };
}

#[macro_export]
macro_rules! prefix_println {
    ($prefix:ident, $($arg:tt)*) => {
        match $prefix {
            Some($crate::log::Prefix::Host) => {
                $crate::println_impl!(println, HOST_PREFIX, $($arg)*);
            }
            Some($crate::log::Prefix::Guest) => {
                $crate::println_impl!(println, GUEST_PREFIX, $($arg)*);
            }
            None => {
                println!($($arg)*);
            }
        }
    };
}
