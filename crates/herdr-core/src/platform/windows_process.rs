//! Windows process observation: native/WoW64 process parameters.

/// The folder and command line Windows keeps in this process's memory.
pub fn about(pid: i32) -> Option<crate::foreground::Foreground> {
    use crate::foreground::Foreground;
    use std::ffi::c_void;
    use std::mem::{MaybeUninit, size_of};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessBasicInformation, ProcessWow64Information,
    };
    use windows_sys::Win32::Foundation::{HANDLE, UNICODE_STRING};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Kernel::STRING32;
    use windows_sys::Win32::System::Threading::*;

    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid as u32) };
    if raw.is_null() {
        return None;
    }
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    let h = process.as_raw_handle() as HANDLE;
    let read = |at: *const c_void, into: *mut c_void, len: usize| {
        let mut n = 0;
        unsafe { ReadProcessMemory(h, at, into, len, &mut n) != 0 && n == len }
    };
    fn value<T>(read: impl Fn(*const c_void, *mut c_void, usize) -> bool, at: *const c_void) -> Option<T> {
        let mut v = MaybeUninit::<T>::uninit();
        read(at, v.as_mut_ptr().cast(), size_of::<T>()).then(|| unsafe { v.assume_init() })
    }
    let text = |s: UNICODE_STRING| -> Option<Vec<u16>> {
        let mut buf = vec![0u16; s.Length as usize / 2];
        read(s.Buffer.cast(), buf.as_mut_ptr().cast(), buf.len() * 2).then_some(buf)
    };

    let mut basic: PROCESS_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let size = size_of::<PROCESS_BASIC_INFORMATION>() as u32;
    let status =
        unsafe { NtQueryInformationProcess(h, ProcessBasicInformation, (&raw mut basic).cast(), size, &mut 0) };
    if status != 0 || basic.PebBaseAddress.is_null() {
        return None;
    }
    let mut peb32 = 0usize;
    if unsafe {
        NtQueryInformationProcess(
            h,
            ProcessWow64Information,
            (&raw mut peb32).cast(),
            size_of::<usize>() as u32,
            &mut 0,
        )
    } != 0
    {
        return None;
    }
    let (dir, image, command_line) = if peb32 != 0 {
        // WoW64 keeps the shell's current directory in its 32-bit PEB, not the native PEB.
        let params: u32 = value(read, (peb32 + 0x10) as *const c_void)?;
        let descriptor = |offset: usize| -> Option<UNICODE_STRING> {
            let s: STRING32 = value(read, (params as usize + offset) as *const c_void)?;
            Some(UNICODE_STRING {
                Length: s.Length,
                MaximumLength: s.MaximumLength,
                Buffer: s.Buffer as usize as *mut u16,
            })
        };
        (descriptor(0x24)?, descriptor(0x38)?, descriptor(0x40)?)
    } else {
        let peb: PEB = value(read, basic.PebBaseAddress.cast())?;
        let params: RTL_USER_PROCESS_PARAMETERS = value(read, peb.ProcessParameters.cast())?;
        // CurrentDirectory.DosPath follows the standard handles in Reserved2.
        let dir = value(read, peb.ProcessParameters.cast::<u8>().wrapping_add(0x38).cast())?;
        (dir, params.ImagePathName, params.CommandLine)
    };
    let mut cwd = String::from_utf16(&text(dir)?).ok()?;
    if cwd.len() > 3 && cwd.ends_with('\\') {
        cwd.pop(); // Windows keeps C:\dir\; only a drive's root has one
    }
    let image = String::from_utf16(&text(image)?).ok()?;
    let name = image.rsplit('\\').next().unwrap_or(&image).to_string();

    let argv = super::split_command_line(text(command_line)?)
        .ok()?
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    Some(Foreground { pid, name, argv, cwd })
}
