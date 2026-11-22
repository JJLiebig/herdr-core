use std::io;

pub(crate) fn writable() -> io::Result<()> {
    #[cfg(unix)]
    {
        // Rust's standard output silently discards EBADF, including for a read-only fd.
        let flags = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if flags & libc::O_ACCMODE == libc::O_RDONLY {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
    }
    Ok(())
}

pub(crate) fn pipe_closed(error: &io::Error) -> bool {
    #[cfg(windows)]
    if error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_PIPE_NOT_CONNECTED as i32) {
        return true;
    }
    error.kind() == io::ErrorKind::BrokenPipe
}
