#[macro_export]
macro_rules! host_println {
    ($($arg:tt)*) => {
        $crate::log::fn_impl::host_println(format!($($arg)*))
    };
}

#[macro_export]
macro_rules! host_eprintln {
    ($($arg:tt)*) => {
        $crate::log::fn_impl::host_eprintln(format!($($arg)*))
    };
}

#[macro_export]
macro_rules! guest_print {
    ($($arg:tt)*) => {
        $crate::log::fn_impl::guest_print(format!($($arg)*))
    };
}

pub mod fn_impl {
    const HOST_PREFIX: &str = "macOS";
    const GUEST_PREFIX: &str = "Linux";

    pub fn host_println(msg: String) {
        println!("{}: {}", HOST_PREFIX, msg)
    }

    pub fn host_eprintln(msg: String) {
        eprintln!("{}: {}", HOST_PREFIX, msg)
    }

    pub fn guest_print(msg: String) {
        print!("{}: {}", GUEST_PREFIX, msg)
    }
}
