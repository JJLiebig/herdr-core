//! Private fork: print a recovery notice inside the PTY before starting its shell.
#[cfg(windows)]
pub(super) const NOTICE_ENV: &str = "HC_RECOVERY_NOTICE";

#[cfg(unix)]
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(windows)]
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn notice_text(notice: &str) -> String {
    // A saved path/argument must not inject terminal control sequences.
    notice.chars().flat_map(|c| if c.is_control() { c.escape_default().collect::<Vec<_>>() } else { vec![c] }).collect()
}

pub(crate) fn recovery_command(argv: &[String]) -> String {
    let args = argv.iter().map(|arg| literal(arg)).collect::<Vec<_>>().join(" ");
    #[cfg(windows)]
    return format!("& {args}");
    #[cfg(unix)]
    args
}

pub(crate) fn recovery_shell(
    shell: &[String],
    notice: &str,
    _env: &mut std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    #[cfg(unix)]
    {
        // Bash's POSIX mode skips EXIT traps on exec failure unless execfail is set.
        let mut argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "printf '%s\\n' {}; if [ -n \"${{BASH_VERSION-}}\" ]; then shopt -s execfail 2>/dev/null || :; fi; trap 'kill -KILL $$' EXIT; exec \"$@\"",
                literal(&notice_text(notice))
            ),
            "sh".into(),
        ];
        argv.extend_from_slice(shell);
        argv
    }
    #[cfg(windows)]
    {
        _env.insert(NOTICE_ENV.into(), notice.into());
        shell.to_vec()
    }
}

#[cfg(windows)]
pub(super) fn notice_command(notice: &str) -> Vec<String> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let exe = std::path::Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    vec![
        exe.display().to_string(),
        "-NoProfile".into(),
        "-Command".into(),
        format!("Write-Output {}", literal(&notice_text(notice))),
    ]
}

#[cfg(windows)]
pub(super) unsafe fn resume_with_notice(
    printer: Option<Vec<u16>>,
    flags: u32,
    env: *mut std::ffi::c_void,
    cwd: *const u16,
    startup: &windows_sys::Win32::System::Threading::STARTUPINFOW,
    job: windows_sys::Win32::Foundation::HANDLE,
    thread: std::os::windows::io::OwnedHandle,
) -> std::io::Result<()> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
    use windows_sys::Win32::System::Threading::{CreateProcessW, INFINITE, ResumeThread, WaitForSingleObject};
    unsafe {
        let Some(mut cmd) = printer else {
            ResumeThread(thread.as_raw_handle().cast());
            return Ok(());
        };
        let mut process = std::mem::zeroed();
        if CreateProcessW(
            std::ptr::null(),
            cmd.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            flags,
            env,
            cwd,
            startup,
            &mut process,
        ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let printer_handle = OwnedHandle::from_raw_handle(process.hProcess as RawHandle);
        let printer_thread = OwnedHandle::from_raw_handle(process.hThread as RawHandle);
        AssignProcessToJobObject(job, printer_handle.as_raw_handle().cast());
        ResumeThread(printer_thread.as_raw_handle().cast());
        // Read the PTY before waiting so a long notice cannot block the printer.
        std::thread::spawn(move || {
            WaitForSingleObject(printer_handle.as_raw_handle().cast(), INFINITE);
            ResumeThread(thread.as_raw_handle().cast());
        });
        Ok(())
    }
}

/// Built-in commands remain argv, so session data never becomes shell source.
pub(crate) fn resume_command(app: &str) -> Option<Vec<String>> {
    let args = match app {
        "codex" => {
            #[cfg(windows)]
            {
                vec!["codex", "--no-daemon", "resume", "{session}"]
            }
            #[cfg(unix)]
            {
                vec!["codex", "resume", "{session}"]
            }
        }
        "claude" => vec!["claude", "--resume", "{session}"],
        _ => return None,
    };
    Some(args.into_iter().map(str::to_owned).collect())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn bash_exec_failure_is_signaled_and_success_preserves_exit_code() {
        use std::os::unix::process::ExitStatusExt;
        for (shell, code, signal) in [
            (vec!["/hc-missing-executable".into()], None, Some(libc::SIGKILL)),
            (vec!["/bin/sh".into(), "-c".into(), "exit 7".into()], Some(7), None),
        ] {
            let mut argv = super::recovery_shell(&shell, "notice", &mut Default::default());
            argv[0] = "/bin/bash".into();
            argv.insert(1, "--posix".into());
            let output = std::process::Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
            assert_eq!(output.status.code(), code);
            assert_eq!(output.status.signal(), signal);
        }
    }

    #[test]
    fn notice_is_literal() {
        let notice = "folder's $(exit)\x1b]2;title\x07\nResume:";
        #[cfg(unix)]
        let argv = super::recovery_shell(
            &["/bin/sh".into(), "-c".into(), "echo shell-output".into()],
            notice,
            &mut Default::default(),
        );
        #[cfg(windows)]
        let argv = super::notice_command(notice);
        let output = std::process::Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let text = String::from_utf8(output.stdout).unwrap().replace("\r\n", "\n");
        #[cfg(unix)]
        let expected = "folder's $(exit)\\u{1b}]2;title\\u{7}\\nResume:\nshell-output\n";
        #[cfg(windows)]
        let expected = "folder's $(exit)\\u{1b}]2;title\\u{7}\\nResume:\n";
        assert_eq!(text, expected);
    }
}
