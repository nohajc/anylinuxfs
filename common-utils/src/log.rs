pub const HOST_PREFIX: &str = "macOS";
pub const GUEST_PREFIX: &str = "Linux";

pub enum Prefix {
    Host,
    Guest,
}

#[macro_export]
macro_rules! println_impl {
    ($print_macro:ident, $prefix:ident, $fmt:expr, $($args:tt)*) => {
        $print_macro!(concat!("{}: ", $fmt), $crate::log::$prefix, $($args)*);
    };
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
macro_rules! guest_print {
    ($($arg:tt)*) => {
        $crate::println_impl!(print, GUEST_PREFIX, $($arg)*);
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
