//! Windows data source: per-process audio sessions of every active capture
//! and render endpoint (`IAudioSessionManager2`), window titles via
//! `EnumWindows`.

use std::collections::HashMap;

use windows::core::{Interface, BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::Media::Audio::{
    eCapture, eRender, AudioSessionStateActive, EDataFlow, IAudioSessionControl2,
    IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

use super::{AudioUser, Direction};

/// Apps with an active audio session: recording on any capture device, or
/// playing on any output device.
pub fn audio_users() -> Vec<AudioUser> {
    // The watcher thread owns this apartment; repeated calls are harmless
    // (S_FALSE once initialized).
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let mut users = Vec::new();
    for (flow, direction) in [
        (eCapture, Direction::Capture),
        (eRender, Direction::Playback),
    ] {
        if let Err(e) = sessions(flow, direction, &mut users) {
            log::debug!("Audio sessions ({direction:?}) unavailable: {e}");
        }
    }
    users
}

fn sessions(
    flow: EDataFlow,
    direction: Direction,
    users: &mut Vec<AudioUser>,
) -> windows::core::Result<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
        for i in 0..devices.GetCount()? {
            let manager: IAudioSessionManager2 = devices.Item(i)?.Activate(CLSCTX_ALL, None)?;
            let list = manager.GetSessionEnumerator()?;
            for j in 0..list.GetCount()? {
                let Ok(session) = list
                    .GetSession(j)
                    .and_then(|s| s.cast::<IAudioSessionControl2>())
                else {
                    continue;
                };
                if session.GetState().ok() != Some(AudioSessionStateActive) {
                    continue;
                }
                // pid 0 is the system sounds session.
                let pid = session.GetProcessId().unwrap_or(0);
                if pid == 0 {
                    continue;
                }
                let app_name = session
                    .GetDisplayName()
                    .map(|name| take_co_string(name))
                    .unwrap_or_default();
                users.push(AudioUser {
                    binary: process_file_name(pid).unwrap_or_default(),
                    app_name,
                    pid: Some(pid),
                    direction,
                });
            }
        }
    }
    Ok(())
}

/// Read and free a string the callee allocated with `CoTaskMemAlloc`.
unsafe fn take_co_string(text: PWSTR) -> String {
    if text.is_null() {
        return String::new();
    }
    let value = unsafe { text.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(text.0 as *const _)) };
    value
}

/// `chrome.exe` for `C:\...\chrome.exe`.
fn process_file_name(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let queried = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        queried.ok()?;
        let path = String::from_utf16_lossy(&buffer[..len as usize]);
        path.rsplit(['\\', '/']).next().map(str::to_string)
    }
}

/// Titles of the visible top-level windows, grouped by owning pid.
pub fn window_titles_by_pid() -> HashMap<u32, Vec<String>> {
    let mut windows: HashMap<u32, Vec<String>> = HashMap::new();
    unsafe {
        let _ = EnumWindows(
            Some(collect_window),
            LPARAM(&mut windows as *mut HashMap<u32, Vec<String>> as isize),
        );
    }
    windows
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the map passed by `window_titles_by_pid`, alive for
    // the whole (synchronous) enumeration.
    let windows = unsafe { &mut *(lparam.0 as *mut HashMap<u32, Vec<String>>) };
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        let mut buffer = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buffer);
        if len <= 0 {
            return BOOL(1);
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let title = String::from_utf16_lossy(&buffer[..len as usize]);
        windows.entry(pid).or_default().push(title);
    }
    BOOL(1)
}
