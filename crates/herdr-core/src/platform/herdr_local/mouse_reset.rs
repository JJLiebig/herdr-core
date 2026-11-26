use crate::foreground::Foreground;

#[cfg(unix)]
pub(crate) fn mouse_root_is_shell(root: &Foreground, _: &[String]) -> bool {
    if root.name != "bash" {
        return crate::foreground::shell_root(root);
    }

    // Bash's startup-file options take operands even for an interactive shell.
    let mut shell = root.clone();
    let mut args = root.argv.iter();
    shell.argv = args.next().cloned().into_iter().collect();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => return args.next().is_none() && crate::foreground::shell_root(&shell),
            "--rcfile" | "--init-file" | "-O" | "+O" | "-o" | "+o" => {
                let _ = args.next();
            }
            _ => shell.argv.push(arg.clone()),
        }
    }
    crate::foreground::shell_root(&shell)
}

#[cfg(windows)]
pub(crate) fn mouse_root_is_shell(root: &Foreground, launch: &[String]) -> bool {
    use crate::platform::windows_foreground::shell;
    // A directly launched batch command can have cmd.exe as its process image.
    if !shell(&root.name)
        || !launch.first().is_some_and(|program| shell(program.rsplit(['/', '\\']).next().unwrap_or(program)))
    {
        return false;
    }

    let name = root.name.to_ascii_lowercase();
    let mut args = root.argv.iter().skip(1);
    match name.strip_suffix(".exe").unwrap_or(&name) {
        "cmd" => {
            // /K runs a command and keeps the shell. /C exits when it ends.
            for arg in args {
                match arg.get(..2).map(str::to_ascii_lowercase).as_deref() {
                    Some("/k") => return true,
                    Some("/c") => return false,
                    _ => {}
                }
            }
            true
        }
        "pwsh" | "powershell" => {
            let mut stays_open = false;
            while let Some(arg) = args.next() {
                let lower = arg.to_ascii_lowercase();
                let (option, value) = lower.split_once(':').map_or((lower.as_str(), None), |(k, v)| (k, Some(v)));
                match option {
                    "-noexit" | "-noe" => stays_open = true,
                    "-command" | "-c" | "-commandwithargs" | "-cwa" | "-encodedcommand" | "-e" | "-ec" | "-enc"
                    | "-file" | "-f" => return stays_open,
                    "-executionpolicy" | "-ep" | "-workingdirectory" | "-wd" | "-configurationname"
                        if value.is_none() =>
                    {
                        let _ = args.next();
                    }
                    _ if !arg.starts_with('-') => return stays_open,
                    _ => {}
                }
            }
            true
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(name: &str, args: &[&str], launched_as: &str) -> bool {
        let root = Foreground {
            pid: 1,
            name: name.into(),
            argv: std::iter::once(name).chain(args.iter().copied()).map(str::to_owned).collect(),
            cwd: String::new(),
        };
        mouse_root_is_shell(&root, &[launched_as.into()])
    }

    #[cfg(unix)]
    #[test]
    fn bash_startup_files_do_not_turn_the_root_into_a_script() {
        assert!(shell("bash", &["--rcfile", "/opt/settings", "-i"], "bash"));
        assert!(shell("bash", &["--init-file", "/opt/settings", "-i"], "bash"));
        assert!(shell("bash", &["-O", "extglob", "-i"], "bash"));
        assert!(!shell("bash", &["+O", "extglob", "script"], "bash"));
        assert!(!shell("bash", &["--rcfile", "/opt/settings", "script"], "bash"));
        assert!(!shell("bash", &["--rcfile", "/opt/settings", "-c", "command"], "bash"));
        assert!(!shell("codex", &[], "codex"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_command_wrappers_are_distinct_from_interactive_shells() {
        assert!(shell("CMD.EXE", &["/d", "/K", "echo ready"], "cmd"));
        assert!(!shell("CMD.EXE", &["/d", "/s", "/C", "command"], "cmd"));
        assert!(!shell("cmd.exe", &[], "codex.cmd"));
        assert!(shell("pwsh.exe", &["-NoExit", "-Command", "command"], r"C:\Program Files\PowerShell\7\pwsh.exe"));
        assert!(!shell("pwsh.exe", &["-File", "script.ps1", "-NoExit"], "pwsh"));
        assert!(!shell("pwsh.exe", &["-NoProfile", "script.ps1"], "pwsh"));
        assert!(shell("pwsh.exe", &["-WorkingDirectory", r"C:\project", "-NoLogo"], "pwsh"));
    }
}
