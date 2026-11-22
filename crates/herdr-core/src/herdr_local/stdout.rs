use crate::platform;
use crate::rpc::RpcError;
use std::fmt;
use std::io::{self, Write};

/// Returns false when the downstream reader has closed the pipe.
pub(crate) fn check(result: io::Result<()>) -> Result<bool, RpcError> {
    match result {
        Ok(()) => Ok(true),
        Err(error) if platform::stdout::pipe_closed(&error) => Ok(false),
        Err(error) => Err(RpcError::internal(format!("failed printing to stdout: {error}"))),
    }
}

pub(crate) fn write_fmt(args: fmt::Arguments<'_>) -> Result<bool, RpcError> {
    buffered(|out| out.write_fmt(args))
}

pub(crate) fn write_bytes(bytes: &[u8]) -> Result<bool, RpcError> {
    if bytes.is_empty() {
        return Ok(true);
    }
    buffered(|out| out.write_all(bytes))
}

pub(crate) fn print_clap(error: &clap::Error) -> Result<bool, RpcError> {
    check(platform::stdout::writable().and_then(|()| error.print()).and_then(|()| io::stdout().flush()))
}

fn buffered(write: impl FnOnce(&mut io::StdoutLock<'_>) -> io::Result<()>) -> Result<bool, RpcError> {
    let mut out = io::stdout().lock();
    check(platform::stdout::writable().and_then(|()| write(&mut out)).and_then(|()| out.flush()))
}

/// Protocol frames and terminal bytes must reach the reader without waiting for a newline.
pub(crate) fn write_raw(bytes: &[u8]) -> io::Result<()> {
    platform::console::stdout().write_all(bytes)
}
