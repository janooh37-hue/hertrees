//! The `os.path` and `pathlib` answers used by the popup.
//! `basename("a/")` is "", matching Python rather than `Path::file_name`.

#[cfg(windows)]
fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

#[cfg(not(windows))]
fn is_sep(c: char) -> bool {
    c == '/'
}

/// `os.path.basename(path)`.
pub fn basename(path: &str) -> &str {
    #[cfg(windows)]
    let path = splitdrive(path).1;
    match path.rfind(is_sep) {
        Some(index) => &path[index + 1..],
        None => path,
    }
}

#[cfg(windows)]
fn splitdrive(path: &str) -> (&str, &str) {
    let chars: Vec<char> = path.chars().take(3).collect();
    if chars.len() >= 2 && chars[1] == ':' {
        let split = chars[0].len_utf8() + 1;
        return (&path[..split], &path[split..]);
    }
    if path.starts_with("\\\\") || path.starts_with("//") {
        // \\server\share: the drive is the first two components.
        let rest = &path[2..];
        if let Some(first) = rest.find(is_sep) {
            let after = &rest[first + 1..];
            let second = after.find(is_sep).map(|i| 2 + first + 1 + i).unwrap_or(path.len());
            return (&path[..second], &path[second..]);
        }
    }
    ("", path)
}

/// `os.path.expanduser("~")`.
pub fn home() -> Option<String> {
    #[cfg(not(windows))]
    {
        let home = match std::env::var_os("HOME") {
            Some(value) => value.to_string_lossy().into_owned(),
            None => passwd_home()?,
        };
        let trimmed = home.trim_end_matches('/');
        Some(if trimmed.is_empty() { "/".to_string() } else { trimmed.to_string() })
    }
    #[cfg(windows)]
    {
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return Some(profile.to_string_lossy().into_owned());
        }
        let path = std::env::var_os("HOMEPATH")?.to_string_lossy().into_owned();
        let drive = std::env::var_os("HOMEDRIVE").map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        Some(drive + &path)
    }
}

#[cfg(not(windows))]
fn passwd_home() -> Option<String> {
    // getpwuid_r: this runs on worker threads, where getpwuid is unsafe.
    let mut size = 1024usize;
    loop {
        let mut buffer = vec![0 as libc::c_char; size];
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        let code = unsafe { libc::getpwuid_r(libc::getuid(), &mut entry, buffer.as_mut_ptr(), size, &mut found) };
        if code == libc::ERANGE && size < 1 << 20 {
            size *= 2;
            continue;
        }
        if code != 0 || found.is_null() || entry.pw_dir.is_null() {
            return None;
        }
        let dir = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
        return Some(dir.to_string_lossy().into_owned());
    }
}

/// `Path.home()`.
pub fn path_home() -> String {
    home().unwrap_or_else(|| "~".to_string())
}

