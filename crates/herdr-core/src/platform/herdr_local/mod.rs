//! Private fork: print a recovery notice inside the PTY before starting its shell.
#[cfg(unix)]
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(windows)]
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
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
    env: &std::collections::BTreeMap<String, String>,
    cwd: &str,
) -> Vec<String> {
    // Let the holder report a shell spawn failure normally, rather than wrapping it in a successful spawn.
    #[cfg(unix)]
    if !shell_available(shell, env, cwd) {
        return shell.to_vec();
    }
    // Do not let a saved path/argument inject terminal control sequences into the notice.
    let notice: String = notice
        .chars()
        .flat_map(|c| if c.is_control() { c.escape_default().collect::<Vec<_>>() } else { vec![c] })
        .collect();
    #[cfg(unix)]
    {
        let mut argv =
            vec!["sh".into(), "-c".into(), format!("printf '%s\\n' {}; exec \"$@\"", literal(&notice)), "sh".into()];
        argv.extend_from_slice(shell);
        argv
    }
    #[cfg(windows)]
    {
        let _ = cwd;
        let Ok(line) = super::windows::command_line(shell, env) else { return shell.to_vec() };
        let line = String::from_utf16_lossy(&line);
        let line = line.trim_end_matches('\0');
        // The resolved program is a Windows filename: quotes can only delimit it.
        let (program, args) = if let Some(quoted) = line.strip_prefix('"') {
            quoted.split_once('"').expect("quoted program")
        } else {
            line.split_once(' ').unwrap_or((line, ""))
        };
        vec![
            super::own_shell()[0].clone(),
            "-NoProfile".into(),
            "-Command".into(),
            format!(
                "Write-Output {}; $s = New-Object Diagnostics.ProcessStartInfo; \
                 $s.FileName = {}; $s.Arguments = {}; $s.UseShellExecute = $false; \
                 $p = [Diagnostics.Process]::Start($s); $p.WaitForExit(); exit $p.ExitCode",
                literal(&notice),
                literal(program),
                literal(args.trim_start())
            ),
        ]
    }
}

#[cfg(unix)]
fn shell_available(shell: &[String], env: &std::collections::BTreeMap<String, String>, _cwd: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable = |path: std::path::PathBuf| {
        std::path::Path::new(_cwd)
            .join(path)
            .metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if shell[0].contains('/') {
        executable(shell[0].as_str().into())
    } else {
        env.get("PATH").is_some_and(|path| std::env::split_paths(path).any(|dir| executable(dir.join(&shell[0]))))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn notice_is_literal_and_precedes_the_configured_shell() {
        #[cfg(unix)]
        let shell = ["sh", "-c", "printf '%s\\n' \"$1\"", "sh", "shell's \"quoted\" output"].map(String::from);
        #[cfg(windows)]
        let shell = [
            super::super::own_shell()[0].as_str(),
            "-NoProfile",
            "-Command",
            "Write-Output 'shell''s \"quoted\" output'",
        ]
        .map(String::from);
        let env = std::env::vars().collect();
        let cwd = std::env::current_dir().unwrap().display().to_string();
        let argv = super::recovery_shell(&shell, "folder's $(exit)\x1b]2;title\x07\nResume:", &env, &cwd);
        let check = |argv: &[String]| {
            let output = std::process::Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            let text = String::from_utf8(output.stdout).unwrap().replace("\r\n", "\n");
            assert_eq!(text, "folder's $(exit)\\u{1b}]2;title\\u{7}\\nResume:\nshell's \"quoted\" output\n");
        };
        check(&argv);
        #[cfg(windows)]
        {
            let mut legacy = argv;
            legacy[0] = "powershell.exe".into();
            check(&legacy);
        }
        let missing = vec!["hc-missing-shell-24".into()];
        assert_eq!(super::recovery_shell(&missing, "notice", &env, &cwd), missing);
    }
}
