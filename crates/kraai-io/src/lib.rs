#![deny(unsafe_code)]

pub mod fs;
#[cfg(feature = "http")]
pub mod http;
pub mod lock;
pub mod read;
