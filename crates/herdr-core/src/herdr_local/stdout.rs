use crate::rpc::RpcError;
use std::fmt;
use std::io::{self, Write};

/// Returns false when the downstream reader has closed the pipe.
pub(crate) fn check(result: io::Result<()>) -> Result<bool, RpcError> {
    match result {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(false),
        Err(error) => Err(RpcError::internal(format!("failed printing to stdout: {error}"))),
    }
}

pub(crate) fn write_fmt(args: fmt::Arguments<'_>) -> Result<bool, RpcError> {
    buffered(|out| out.write_fmt(args))
}

pub(crate) fn write_bytes(bytes: &[u8]) -> Result<bool, RpcError> {
    buffered(|out| out.write_all(bytes))
}

fn buffered(write: impl FnOnce(&mut io::StdoutLock<'_>) -> io::Result<()>) -> Result<bool, RpcError> {
    let mut out = io::stdout().lock();
    check(write(&mut out).and_then(|()| out.flush()))
}

/// Protocol frames and terminal bytes must reach the reader without waiting for a newline.
pub(crate) fn write_raw(bytes: &[u8]) -> io::Result<()> {
    crate::platform::console::stdout().write_all(bytes)
}
