// Thin Win32 helpers: taskbar theme, tray icon size, fullscreen/game
// detection and toast app-id registration.

use std::{ffi::OsStr, os::windows::ffi::OsStrExt, path::Path, ptr, time::Duration};

use winapi::{
    shared::{minwindef::DWORD, winerror::ERROR_SUCCESS},
    um::{
        shellapi::{
            SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE,
            QUNS_RUNNING_D3D_FULL_SCREEN,
        },
        sysinfoapi::GetTickCount,
        winnt::REG_SZ,
        winreg::{RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD},
        winuser::{GetLastInputInfo, GetSystemMetrics, LASTINPUTINFO, SM_CXSMICON},
    },
};

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// True when the Windows taskbar uses the light theme. Defaults to dark,
/// which is the Windows 11 default.
pub fn taskbar_is_light() -> bool {
    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("SystemUsesLightTheme");
    let mut data: DWORD = 0;
    let mut size = std::mem::size_of::<DWORD>() as DWORD;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            ptr::null_mut(),
            &mut data as *mut DWORD as *mut _,
            &mut size,
        )
    };
    status == ERROR_SUCCESS as i32 && data != 0
}

/// Tray icon size in physical pixels (16 at 100% scaling, 20 at 125%, ...).
pub fn tray_icon_size() -> u32 {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
    size.clamp(16, 64) as u32
}

/// Time since the last keyboard or mouse input in this session.
pub fn idle_time() -> Option<Duration> {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return None;
    }
    // Both are 32-bit tick counts; wrapping_sub survives the 49-day rollover.
    let now = unsafe { GetTickCount() };
    Some(Duration::from_millis(now.wrapping_sub(info.dwTime) as u64))
}

/// True while a fullscreen game (exclusive or borderless), a fullscreen app
/// or presentation mode is active.
pub fn fullscreen_app_running() -> bool {
    let mut state = 0;
    let hr = unsafe { SHQueryUserNotificationState(&mut state) };
    hr >= 0
        && matches!(
            state,
            QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE
        )
}

/// Registers an AppUserModelID for this unpackaged app so toasts show our
/// name and icon instead of "Windows PowerShell".
pub fn register_app_id(app_id: &str, display_name: &str, icon: &Path) -> Result<(), String> {
    let key = wide(&format!(r"Software\Classes\AppUserModelId\{app_id}"));
    let set = |name: &str, value: &str| -> Result<(), String> {
        let name_w = wide(name);
        let value_w = wide(value);
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name_w.as_ptr(),
                REG_SZ,
                value_w.as_ptr() as *const _,
                (value_w.len() * 2) as DWORD,
            )
        };
        if status == ERROR_SUCCESS as i32 {
            Ok(())
        } else {
            Err(format!("RegSetKeyValueW({name}) failed: {status}"))
        }
    };
    set("DisplayName", display_name)?;
    set("IconUri", &icon.to_string_lossy())
}

/// Per-app data directory: %LOCALAPPDATA%\BasiliskPower (created on demand).
/// Holds the log, status.json, the history CSV and the toast icon.
pub fn data_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|root| {
        let dir = std::path::PathBuf::from(root).join("BasiliskPower");
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
}

/// Path of the log file (same directory as status.json and the CSV).
pub fn log_path() -> Option<std::path::PathBuf> {
    data_dir().map(|dir| dir.join("basilisk-power.log"))
}

/// Local wall-clock time as ("HH:MM", "HH:MM:SS", "YYYY-MM-DD").
/// Print a one-line version answer for this GUI exe.
///
/// A Windows GUI-subsystem process has no console of its own and Rust's stdout
/// may be unbound, so this writes to whatever standard-output handle was
/// inherited (a pipe under PowerShell, a console after AttachConsole). If
/// there is no usable handle -- launched from Explorer -- it shows a small
/// message box instead of exiting silently.
pub fn show_version(text: &str) {
    #[cfg(windows)]
    {
        use std::ffi::c_void;

        const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
        const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;
        const MB_ICONINFORMATION: u32 = 0x40;

        #[link(name = "kernel32")]
        extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
            fn GetStdHandle(which: u32) -> *mut c_void;
            fn WriteFile(
                handle: *mut c_void,
                buffer: *const u8,
                bytes: u32,
                written: *mut u32,
                overlapped: *mut c_void,
            ) -> i32;
        }

        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(
                hwnd: *mut c_void,
                text: *const u16,
                caption: *const u16,
                utype: u32,
            ) -> i32;
        }

        unsafe {
            // ATTACH_PARENT_PROCESS; harmless when there is no parent console.
            AttachConsole(u32::MAX);

            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut line = String::from(text);
            line.push_str("\r\n");
            let mut written = 0u32;
            let wrote = !handle.is_null()
                && handle != INVALID_HANDLE_VALUE
                && WriteFile(
                    handle,
                    line.as_ptr(),
                    line.len() as u32,
                    &mut written,
                    std::ptr::null_mut(),
                ) != 0
                && written > 0;

            if !wrote {
                let wide = |s: &str| -> Vec<u16> {
                    use std::os::windows::ffi::OsStrExt;
                    std::ffi::OsStr::new(s)
                        .encode_wide()
                        .chain(std::iter::once(0))
                        .collect()
                };
                let text: Vec<u16> = wide(text);
                let caption: Vec<u16> = wide("Basilisk Power");
                MessageBoxW(
                    std::ptr::null_mut(),
                    text.as_ptr(),
                    caption.as_ptr(),
                    MB_ICONINFORMATION,
                );
            }
        }
    }

    #[cfg(not(windows))]
    println!("{text}");
}

/// Report a startup error the same way: stderr when possible, otherwise a
/// message box, so an invalid flag never exits silently.
pub fn show_error(text: &str) {
    #[cfg(windows)]
    {
        use std::ffi::c_void;

        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(
                hwnd: *mut c_void,
                text: *const u16,
                caption: *const u16,
                utype: u32,
            ) -> i32;
        }

        let wide = |s: &str| -> Vec<u16> {
            use std::os::windows::ffi::OsStrExt;
            std::ffi::OsStr::new(s)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        };
        let text: Vec<u16> = wide(text);
        let caption: Vec<u16> = wide("Basilisk Power");
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                caption.as_ptr(),
                0x10, // MB_ICONERROR
            );
        }
    }

    #[cfg(not(windows))]
    eprintln!("{text}");
}

pub fn local_time_parts() -> (String, String, String) {
    use winapi::um::minwinbase::SYSTEMTIME;
    use winapi::um::sysinfoapi::GetLocalTime;
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    (
        format!("{:02}:{:02}", st.wHour, st.wMinute),
        format!("{:02}:{:02}:{:02}", st.wHour, st.wMinute, st.wSecond),
        format!("{:04}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay),
    )
}

/// Opens a file or folder with the default shell association.
pub fn open_path(path: &Path) -> Result<(), String> {
    use winapi::um::shellapi::ShellExecuteW;
    use winapi::um::winuser::SW_SHOWNORMAL;
    let verb = wide("open");
    let file = wide(&path.to_string_lossy());
    let res = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL as i32,
        )
    };
    if res as isize > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW failed: {res:?}"))
    }
}

/// Single-instance guard. Returns false when another instance already
/// runs (the caller should exit). The mutex handle is intentionally never
/// closed — it must stay held for the process lifetime; the OS releases it
/// when the process exits, so a crash can never leave the app locked out.
pub fn acquire_single_instance_lock() -> bool {
    use winapi::shared::winerror::ERROR_ALREADY_EXISTS;
    use winapi::um::synchapi::CreateMutexW;
    let name = wide(r"Local\BasiliskPower.SingleInstance");
    let handle = unsafe { CreateMutexW(ptr::null_mut(), 0, name.as_ptr()) };
    if handle.is_null() {
        // Can't create the mutex (permissions?) — don't block the app.
        return true;
    }
    unsafe { winapi::um::errhandlingapi::GetLastError() != ERROR_ALREADY_EXISTS }
}
