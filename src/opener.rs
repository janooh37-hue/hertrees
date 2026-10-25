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
