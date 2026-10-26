//! Host operating system differences.
//!
//! Herdr runs on Linux, macOS and Windows. Everything the bar does differently
//! per platform lives here, so the modules above read the same everywhere:
//!
//! * the Windows named pipe behind `HERDR_SOCKET_PATH`,
//! * detaching a helper process from the popup,
//! * owner-only files.
//!
//! The terminal itself is the one exception: it has a backend per platform
//! under `term`.

use std::io;

pub const WINDOWS: bool = cfg!(windows);

/// Python's `str(OSError)`: "[Errno 2] No such file or directory".
pub fn os_error_str(error: &io::Error) -> String {
    if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) {
        // A socket timeout: Python's socket.timeout says just this.
        return "timed out".into();
    }
    match error.raw_os_error() {
        Some(code) => {
            let text = error.to_string();
            let suffix = format!(" (os error {})", code);
            let message = text.strip_suffix(&suffix).unwrap_or(&text);
            if cfg!(windows) {
                format!("[WinError {}] {}", code, message)
            } else {
                format!("[Errno {}] {}", code, message)
            }
        }
        None => error.to_string(),
    }
}

// -- the socket --------------------------------------------------------------

pub const PIPE_PREFIX: &str = "\\\\.\\pipe\\";

/// The named pipe behind a Windows socket path.
///
/// Herdr serves its Windows API endpoint in the pipe namespace under the
/// socket path string itself: `C:\Users\me\AppData\Roaming\herdr\herdr.sock`
/// is reached at `\\.\pipe\C:\Users\me\AppData\Roaming\herdr\herdr.sock`.
/// The file at the path is only a marker that says which server owns it.
pub fn pipe_name(socket_path: &str) -> String {
    if socket_path.starts_with(PIPE_PREFIX) {
        return socket_path.to_string();
    }
    format!("{}{}", PIPE_PREFIX, socket_path)
}

#[cfg(windows)]
mod pipe {
    use std::io;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_MORE_DATA,
        ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, SECURITY_IDENTIFICATION,
        SECURITY_SQOS_PRESENT,
    };
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

    /// How long a pipe may have no listening instance before the server counts
    /// as gone: Herdr opens the next instance right after accepting a client.
    const PIPE_RELISTEN_GRACE: Duration = Duration::from_millis(250);

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn remaining_ms(deadline: Instant) -> u32 {
        deadline.saturating_duration_since(Instant::now()).as_millis().min(u32::MAX as u128) as u32
    }

    /// One request over a named pipe; the first reply line, without newline.
    ///
    /// Every step shares one deadline, as a socket timeout bounds each call:
    /// the pipe is opened for overlapped I/O, because a plain read would wait
    /// forever on a server that stopped answering.
    pub fn request(name: &str, payload: &[u8], timeout: f64) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
        let handle = open(name, deadline)?;
        transfer(&handle, true, payload.as_ptr() as *mut u8, payload.len() as u32, deadline)?;
        let mut incoming = vec![0u8; 65536];
        let mut buffered: Vec<u8> = Vec::new();
        while !buffered.contains(&b'\n') {
            let count = transfer(&handle, false, incoming.as_mut_ptr(), incoming.len() as u32, deadline)?;
            if count == 0 {
                break; // the server closed its end
            }
            buffered.extend_from_slice(&incoming[..count as usize]);
        }
        let end = buffered.iter().position(|&b| b == b'\n').unwrap_or(buffered.len());
        buffered.truncate(end);
        Ok(buffered)
    }

    fn open(name: &str, deadline: Instant) -> io::Result<Handle> {
        let grace = Instant::now() + PIPE_RELISTEN_GRACE;
        let flags = FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;
        let name = wide(name);
        loop {
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    flags,
                    std::ptr::null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                return Ok(Handle(handle));
            }
            let code = unsafe { GetLastError() };
            let now = Instant::now();
            if code == ERROR_PIPE_BUSY && now < deadline {
                // Every instance is serving someone; wait for the server to offer one.
                unsafe {
                    WaitNamedPipeW(name.as_ptr(), remaining_ms(deadline).max(1));
                }
            } else if code == ERROR_FILE_NOT_FOUND && now < deadline.min(grace) {
                // Between two instances, or gone for good: only a short wait tells.
                std::thread::sleep(Duration::from_millis(10));
            } else {
                return Err(io::Error::from_raw_os_error(code as i32));
            }
        }
    }

    /// One overlapped ReadFile or WriteFile; bytes moved, 0 once the pipe closed.
    fn transfer(handle: &Handle, write: bool, buffer: *mut u8, size: u32, deadline: Instant) -> io::Result<u32> {
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let event = Handle(event);
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = event.0;
        let mut done: u32 = 0;
        let ok = unsafe {
            if write {
                WriteFile(handle.0, buffer, size, std::ptr::null_mut(), &mut overlapped)
            } else {
                ReadFile(handle.0, buffer, size, std::ptr::null_mut(), &mut overlapped)
            }
        };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            if code == ERROR_BROKEN_PIPE {
                return Ok(0);
            }
            if code != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(code as i32));
            }
            if unsafe { WaitForSingleObject(event.0, remaining_ms(deadline)) } != WAIT_OBJECT_0 {
                unsafe {
                    CancelIoEx(handle.0, &overlapped);
                    // The kernel owns the buffer until the cancelled call lands.
                    GetOverlappedResult(handle.0, &overlapped, &mut done, 1);
                }
                return Err(io::Error::new(io::ErrorKind::TimedOut, "timed out waiting for herdr"));
            }
        }
        if unsafe { GetOverlappedResult(handle.0, &overlapped, &mut done, 0) } == 0 {
            let code = unsafe { GetLastError() };
            if code == ERROR_BROKEN_PIPE {
                return Ok(0);
            }
            if code != ERROR_MORE_DATA {
                // a message-mode pipe hands it over in parts
                return Err(io::Error::from_raw_os_error(code as i32));
            }
        }
        Ok(done)
    }
}

#[cfg(windows)]
pub fn pipe_request(name: &str, payload: &[u8], timeout: f64) -> io::Result<Vec<u8>> {
    pipe::request(name, payload, timeout)
}

#[cfg(not(windows))]
pub fn pipe_request(_name: &str, _payload: &[u8], _timeout: f64) -> io::Result<Vec<u8>> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "named pipes exist only on Windows"))
}

// -- detached helpers and private files ---------------------------------------

/// Start `command` outside the popup's session, so it outlives the popup.
#[cfg(unix)]
pub fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // Only an async-signal-safe syscall is used between fork and exec.
    unsafe {
        command.pre_exec(|| if libc::setsid() == -1 { Err(io::Error::last_os_error()) } else { Ok(()) });
    }
}

/// Start `command` without the popup's console, so it outlives the popup.
#[cfg(windows)]
pub fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Make a file `options` creates readable by its owner only (0600). On
/// Windows it inherits the ACL of its directory, which under the user
/// profile already shuts out other users.
#[cfg_attr(not(unix), allow(unused_variables))]
pub fn owner_only(options: &mut std::fs::OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pipe_name_is_the_socket_path_under_the_pipe_namespace() {
        let path = r"C:\Users\me\AppData\Roaming\herdr\herdr.sock";
        assert_eq!(pipe_name(path), format!("{}{}", r"\\.\pipe\", path));
        assert_eq!(pipe_name(&pipe_name(path)), pipe_name(path));
    }
}
