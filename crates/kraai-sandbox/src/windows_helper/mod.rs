mod client;
mod firewall;
mod identity;
mod journal;
mod service;
mod setup;
#[cfg(test)]
mod tests;

use std::io;

pub(crate) use client::Lease;
pub(crate) use identity::profile_name;

const SERVICE: &str = "KraaiSandbox";
const PIPE: &str = r"\\.\pipe\KraaiSandbox.v1";
const MAGIC: &[u8; 4] = b"KSB1";
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub fn main() -> io::Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [mode] if mode == "install" => setup::install(),
        [mode] if mode == "uninstall" => setup::uninstall(),
        [mode] if mode == "service" => service::dispatch(),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: kraai-sandbox-helper install|uninstall; installation requires an administrator terminal",
        )),
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

fn check(code: u32) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}
