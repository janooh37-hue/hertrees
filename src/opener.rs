//! Find the Herdr binary, and open the picker's popup through it.

pub const POPUP_ARGS: &[&str] =
    &["plugin", "pane", "open", "--plugin", "hertrees", "--entrypoint", "picker", "--placement", "popup"];

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

/// `hertrees --open`: replace this process with `herdr plugin pane open …`.
pub fn open_picker() -> i32 {
    let mut process = std::process::Command::new(herdr_bin());
    process.args(POPUP_ARGS);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = process.exec();
        eprintln!("hertrees: could not run herdr: {}", crate::host::os_error_str(&error));
        2
    }
    #[cfg(not(unix))]
    {
        match process.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(error) => {
                eprintln!("hertrees: could not run herdr: {}", crate::host::os_error_str(&error));
                2
            }
        }
    }
}
