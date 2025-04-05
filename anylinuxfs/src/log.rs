macro_rules! host_println {
    ($($arg:tt)*) => {
        println!("macOS: {}", format!($($arg)*))
    };
}

pub(crate) use host_println;

macro_rules! host_eprintln {
    ($($arg:tt)*) => {
        eprintln!("macOS: {}", format!($($arg)*))
    };
}

pub(crate) use host_eprintln;
