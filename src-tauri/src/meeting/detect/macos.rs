//! macOS data source. Who does audio IO comes from Core Audio's process
//! objects (macOS 14.2+); on older systems, from the default input being
//! "running somewhere", attributed to the running apps. Window titles come
//! from `CGWindowListCopyWindowInfo`; they need the Screen Recording
//! permission, and without it only native call apps are detected.

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{self, NonNull};

use objc2_app_kit::NSWorkspace;
use objc2_core_audio::{
    kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioProcessPropertyBundleID,
    kAudioProcessPropertyIsRunningInput, kAudioProcessPropertyIsRunningOutput,
    kAudioProcessPropertyPID, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectPropertySelector,
};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_graphics::{
    kCGNullWindowID, kCGWindowName, kCGWindowOwnerPID, CGWindowListCopyWindowInfo,
    CGWindowListOption,
};

use super::{AudioUser, Direction};

const SYSTEM_OBJECT: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

pub fn audio_users() -> Vec<AudioUser> {
    match process_objects() {
        Some(objects) => objects.into_iter().flat_map(process_users).collect(),
        None => running_apps_if_mic_in_use(),
    }
}

fn address(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// A fixed-size property value.
fn get<T: Copy + Default>(
    object: AudioObjectID,
    selector: AudioObjectPropertySelector,
) -> Option<T> {
    let mut address = address(selector);
    let mut value = T::default();
    let mut size = size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    (status == 0).then_some(value)
}

/// Every process known to the audio server; `None` before macOS 14.2.
fn process_objects() -> Option<Vec<AudioObjectID>> {
    let mut address = address(kAudioHardwarePropertyProcessObjectList);
    let mut size = 0u32;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            SYSTEM_OBJECT,
            NonNull::from(&mut address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 {
        return None;
    }
    let mut objects = vec![0 as AudioObjectID; size as usize / size_of::<AudioObjectID>()];
    if objects.is_empty() {
        return Some(objects);
    }
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            NonNull::from(&mut address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(objects.as_mut_ptr().cast::<c_void>())?,
        )
    };
    objects.truncate(size as usize / size_of::<AudioObjectID>());
    (status == 0).then_some(objects)
}

/// One entry per direction the process is doing IO in.
fn process_users(object: AudioObjectID) -> Vec<AudioUser> {
    let Some(pid) = get::<i32>(object, kAudioProcessPropertyPID) else {
        return Vec::new();
    };
    let directions = [
        (kAudioProcessPropertyIsRunningInput, Direction::Capture),
        (kAudioProcessPropertyIsRunningOutput, Direction::Playback),
    ]
    .into_iter()
    .filter(|(selector, _)| get::<u32>(object, *selector).is_some_and(|running| running != 0))
    .map(|(_, direction)| direction)
    .collect::<Vec<_>>();
    if directions.is_empty() {
        return Vec::new();
    }
    let binary = process_file_name(pid).unwrap_or_default();
    let app_name = bundle_id(object).unwrap_or_default();
    directions
        .into_iter()
        .map(|direction| AudioUser {
            binary: binary.clone(),
            app_name: app_name.clone(),
            pid: u32::try_from(pid).ok(),
            direction,
        })
        .collect()
}

fn bundle_id(object: AudioObjectID) -> Option<String> {
    let mut address = address(kAudioProcessPropertyBundleID);
    let mut value: *mut CFString = ptr::null_mut();
    let mut size = size_of::<*mut CFString>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    if status != 0 {
        return None;
    }
    // SAFETY: the property hands over a +1 CFString the caller releases.
    let value = unsafe { CFRetained::from_raw(NonNull::new(value)?) };
    Some(value.to_string())
}

/// `zoom.us` for `/Applications/zoom.us.app/Contents/MacOS/zoom.us`.
fn process_file_name(pid: i32) -> Option<String> {
    let mut buffer = vec![0u8; 4096];
    let len = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if len <= 0 {
        return None;
    }
    buffer.truncate(len as usize);
    let path = String::from_utf8_lossy(&buffer);
    path.rsplit('/').next().map(str::to_string)
}

/// Before macOS 14.2 the audio server does not say who records: when the
/// default input is in use, every running app is a candidate recorder, and
/// [`super::classify`] keeps only call apps (and browsers in a meeting).
fn running_apps_if_mic_in_use() -> Vec<AudioUser> {
    let in_use = get::<AudioObjectID>(SYSTEM_OBJECT, kAudioHardwarePropertyDefaultInputDevice)
        .and_then(|device| get::<u32>(device, kAudioDevicePropertyDeviceIsRunningSomewhere))
        .is_some_and(|running| running != 0);
    if !in_use {
        return Vec::new();
    }
    objc2::rc::autoreleasepool(|_| {
        NSWorkspace::sharedWorkspace()
            .runningApplications()
            .iter()
            .map(|app| AudioUser {
                binary: app
                    .executableURL()
                    .and_then(|url| url.lastPathComponent())
                    .map(|name| name.to_string())
                    .unwrap_or_default(),
                app_name: app
                    .bundleIdentifier()
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
                pid: u32::try_from(app.processIdentifier()).ok(),
                direction: Direction::Capture,
            })
            .collect()
    })
}

/// Titles of the on-screen windows, grouped by owning pid. Windows of other
/// apps come without a title unless Screen Recording is allowed.
pub fn window_titles_by_pid() -> HashMap<u32, Vec<String>> {
    let mut windows: HashMap<u32, Vec<String>> = HashMap::new();
    let Some(list) = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
        kCGNullWindowID,
    ) else {
        return windows;
    };
    // SAFETY: the window list is an array of dictionaries keyed by strings.
    let list =
        unsafe { CFRetained::cast_unchecked::<CFArray<CFDictionary<CFString, CFType>>>(list) };
    let (pid_key, name_key) = unsafe { (kCGWindowOwnerPID, kCGWindowName) };
    for window in list.iter() {
        let pid = window
            .get(pid_key)
            .and_then(|pid| pid.downcast::<CFNumber>().ok())
            .and_then(|pid| pid.as_i64())
            .and_then(|pid| u32::try_from(pid).ok());
        let title = window
            .get(name_key)
            .and_then(|name| name.downcast::<CFString>().ok())
            .map(|name| name.to_string())
            .filter(|name| !name.is_empty());
        if let (Some(pid), Some(title)) = (pid, title) {
            windows.entry(pid).or_default().push(title);
        }
    }
    windows
}
