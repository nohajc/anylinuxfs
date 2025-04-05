#[macro_export]
macro_rules! host_println {
    ($($arg:tt)*) => {
        println!("macOS: {}", format!($($arg)*))
    };
}

#[macro_export]
macro_rules! host_eprintln {
    ($($arg:tt)*) => {
        eprintln!("macOS: {}", format!($($arg)*))
    };
}
