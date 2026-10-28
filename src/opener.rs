//! Find the Herdr binary, and open the picker's popup through Herdr or tmux.

pub const POPUP_ARGS: &[&str] =
    &["plugin", "pane", "open", "--plugin", "hertrees", "--entrypoint", "picker", "--placement", "popup"];

/// tmux's popup with the plugin's geometry (`herdr-plugin.toml`), started in
/// the focused pane's directory so the picker shows that pane's project.
pub const TMUX_POPUP_ARGS: &[&str] = &["display-popup", "-E", "-w", "74%", "-h", "62%", "-d", "#{pane_current_path}", "--"];

/// The Herdr binary to shell out to.
///
/// HERDR_BIN_PATH can outlive the binary it names -- upgrading Herdr under a
/// running server leaves the old path dangling -- so an unusable value falls
/// back to whatever `herdr` resolves to on PATH.
pub fn herdr_bin() -> String {
    if let Some(candidate) = std::env::var_os("HERDR_BIN_PATH") {
        if !candidate.is_empty() && is_executable_file(std::path::Path::new(&candidate)) {
            return candidate.to_string_lossy().into_owned();
        }
    }
    "herdr".into()
}

#[cfg(unix)]
pub fn is_executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(windows)]
pub fn is_executable_file(path: &std::path::Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

/// Whether `name` is a program on PATH. Windows runs it by extension, so
/// there each `PATHEXT` suffix is tried (`claude.exe`, `omp.cmd`).
pub fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    let suffixes: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';').filter(|suffix| !suffix.is_empty()).map(str::to_owned).collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path).any(|dir| {
        suffixes.iter().any(|suffix| is_executable_file(&dir.join(format!("{name}{suffix}"))))
    })
}

/// `hertrees --open`: replace this process with `herdr plugin pane open …`,
/// or under tmux with `tmux display-popup … hertrees`.
pub fn open_picker() -> i32 {
    let (program, mut process) = match crate::mux::kind_from_env() {
        crate::mux::Kind::Herdr => {
            let mut process = std::process::Command::new(herdr_bin());
            process.args(POPUP_ARGS);
            ("herdr", process)
        }
        crate::mux::Kind::Tmux => {
            let executable = match std::env::current_exe() {
                Ok(executable) => executable,
                Err(error) => {
                    eprintln!("hertrees: cannot find this executable: {}", crate::host::os_error_str(&error));
                    return 2;
                }
            };
            let mut process = std::process::Command::new("tmux");
            process.args(TMUX_POPUP_ARGS).arg(executable);
            ("tmux", process)
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = process.exec();
        eprintln!("hertrees: could not run {program}: {}", crate::host::os_error_str(&error));
        2
    }
    #[cfg(not(unix))]
    {
        match process.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(error) => {
                eprintln!("hertrees: could not run {program}: {}", crate::host::os_error_str(&error));
                2
            }
        }
    }
}
