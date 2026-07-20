pub mod args;

#[cfg(not(windows))]
pub mod compress;
#[cfg(windows)]
pub mod compress {
    include!(concat!(env!("OUT_DIR"), "/compress_windows.rs"));
}

pub mod crypto;
pub mod csv;
pub mod dirs;

#[cfg(not(windows))]
pub mod files;
#[cfg(windows)]
pub mod files {
    include!(concat!(env!("OUT_DIR"), "/files_windows.rs"));
}

pub mod util;
