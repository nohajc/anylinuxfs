use std::{
    fs::File,
    io::{self, BufRead, Seek},
    path::Path,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

pub const HOST_PREFIX: &str = "macOS: ";
pub const GUEST_PREFIX: &str = "Linux: ";
pub const EMPTY_PREFIX: &str = "";

pub static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();
pub static CONSOLE_LOG_ENABLED: AtomicBool = AtomicBool::new(true);
pub static PRINTED_LINES: AtomicUsize = AtomicUsize::new(0);

pub fn init_log_file(path: impl AsRef<Path>) -> io::Result<()> {
    let log_file = File::options()
        .write(true)
        .read(true)
        .create(true)
        .truncate(true)
        .open(&path)?;
    LOG_FILE.get_or_init(|| Mutex::new(log_file));
    PRINTED_LINES.store(0, Ordering::Relaxed);
    Ok(())
}

pub fn enable_console_log() {
    CONSOLE_LOG_ENABLED.store(true, Ordering::Relaxed);
}

pub fn disable_console_log() {
    CONSOLE_LOG_ENABLED.store(false, Ordering::Relaxed);
}

pub fn print_log_file() {
    if let Some(log_file) = LOG_FILE.get() {
        let mut log_file = log_file.lock().unwrap();
        log_file.seek(io::SeekFrom::Start(0)).unwrap();
        let log_reader = io::BufReader::new(&*log_file);

        for (i, line) in log_reader.lines().enumerate() {
            if i < PRINTED_LINES.load(Ordering::Relaxed) {
                continue;
            }
            if let Ok(line) = line {
                println!("{}", line);
            }
        }
    }
}

pub enum Prefix {
    Host,
    Guest,
}

#[macro_export]
macro_rules! println_impl {
    ($print_macro:ident, $prefix:ident, $fmt:expr, $($args:tt)*) => {{
        let res1: anyhow::Result<()> = if $crate::log::CONSOLE_LOG_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
            $crate::log::PRINTED_LINES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            $crate::$print_macro!(concat!("{}", $fmt), $crate::log::$prefix, $($args)*)
        } else {
            Ok(())
        }.map_err(|e| e.into());
        let res2: anyhow::Result<()> = if let Some(log_file) = $crate::log::LOG_FILE.get() {
            use std::io::Write;
            let mut log_file = log_file.lock().unwrap();
            writeln!(&mut log_file, concat!("{}", $fmt), $crate::log::$prefix, $($args)*)
        } else {
            Ok(())
        }.map_err(|e| e.into());
        res1.and(res2)
    }};
    ($print_macro:ident, $prefix:ident, $fmt:expr) => {
        $crate::println_impl!($print_macro, $prefix, $fmt, )
    };
}

#[macro_export]
macro_rules! host_println {
    ($($arg:tt)*) => {
        _ = $crate::println_impl!(safe_println, HOST_PREFIX, $($arg)*)
    };
}

#[macro_export]
macro_rules! host_eprintln {
    ($($arg:tt)*) => {
        _ = $crate::println_impl!(safe_eprintln, HOST_PREFIX, $($arg)*)
    };
}

#[macro_export]
macro_rules! guest_println {
    ($($arg:tt)*) => {
        _ = $crate::println_impl!(safe_println, GUEST_PREFIX, $($arg)*)
    };
}

#[macro_export]
macro_rules! prefix_println {
    ($prefix:ident, $($arg:tt)*) => {
        _ = match $prefix {
            Some($crate::log::Prefix::Host) => {
                $crate::println_impl!(safe_println, HOST_PREFIX, $($arg)*)
            }
            Some($crate::log::Prefix::Guest) => {
                $crate::println_impl!(safe_println, GUEST_PREFIX, $($arg)*)
            }
            None => {
                $crate::println_impl!(safe_println, EMPTY_PREFIX, $($arg)*)
            }
        }
    };
}

#[macro_export]
macro_rules! prefix_eprintln {
    ($prefix:ident, $($arg:tt)*) => {
        _ = match $prefix {
            Some($crate::log::Prefix::Host) => {
                $crate::println_impl!(safe_eprintln, HOST_PREFIX, $($arg)*)
            }
            Some($crate::log::Prefix::Guest) => {
                $crate::println_impl!(safe_eprintln, GUEST_PREFIX, $($arg)*)
            }
            None => {
                $crate::println_impl!(safe_eprintln, EMPTY_PREFIX, $($arg)*)
            }
        }
    };
}

#[derive(Debug)]
pub struct PrintError(pub io::Error);

impl std::fmt::Display for PrintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PrintError {}

impl PrintError {
    pub fn broken_pipe(&self) -> bool {
        self.0.kind() == io::ErrorKind::BrokenPipe
    }
}

#[macro_export]
macro_rules! safe_println {
    ($($arg:tt)*) => {{
        use std::io::Write;
        writeln!(std::io::stdout(), $($arg)*).map_err($crate::log::PrintError)
    }};
}

#[macro_export]
macro_rules! safe_eprintln {
    ($($arg:tt)*) => {{
        use std::io::Write;
        writeln!(std::io::stderr(), $($arg)*).map_err($crate::log::PrintError)
    }};
}
