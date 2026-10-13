//! System audio on macOS: a Core Audio process tap of everything being
//! played, exposed through a private aggregate device that cpal records from
//! like any other input.
//!
//! Process taps exist since macOS 14.2. The tap functions are looked up at
//! runtime instead of linked, so the app still launches on older systems and
//! meeting mode reports the requirement there. The first tap asks the user for
//! the "System Audio Recording" permission (`NSAudioCaptureUsageDescription`
//! in Info.plist).

use std::ffi::{c_void, CStr};
use std::ptr::{self, NonNull};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use log::warn;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioHardwarePropertyDefaultSystemOutputDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioSubDeviceUIDKey,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, AudioHardwareCreateAggregateDevice,
    AudioHardwareDestroyAggregateDevice, AudioObjectGetPropertyData, AudioObjectID,
    AudioObjectPropertyAddress, CATapDescription,
};
use objc2_core_foundation::{CFDictionary, CFRetained, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use super::cpal_source::CpalSource;
use super::AudioSource;

/// Name of the aggregate device; cpal finds it by this name.
const DEVICE_NAME: &str = "Handy Meeting Audio";
/// The aggregate device can take a moment to show up in the device list.
const FIND_ATTEMPTS: u32 = 20;
const FIND_INTERVAL: Duration = Duration::from_millis(50);

type OSStatus = i32;
type CreateTapFn =
    unsafe extern "C-unwind" fn(*const CATapDescription, *mut AudioObjectID) -> OSStatus;
type DestroyTapFn = unsafe extern "C-unwind" fn(AudioObjectID) -> OSStatus;

pub fn open_system_audio() -> Result<Box<dyn AudioSource>> {
    let tap = SystemTap::create()?;
    for _ in 0..FIND_ATTEMPTS {
        if let Some(device) = CpalSource::find_input(DEVICE_NAME)? {
            return Ok(Box::new(CpalSource::open_input(&device, Box::new(tap))?));
        }
        thread::sleep(FIND_INTERVAL);
    }
    bail!("The system audio capture device did not appear")
}

/// The tap and the aggregate device around it; both are destroyed on drop.
struct SystemTap {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    destroy_tap: DestroyTapFn,
}

impl SystemTap {
    fn create() -> Result<Self> {
        let (create_tap, destroy_tap) = tap_functions().ok_or_else(|| {
            anyhow!("Recording the other participants requires macOS 14.2 or later")
        })?;

        // Stereo mix of every process's output; nothing excluded.
        let description = unsafe {
            let description = CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &NSArray::new(),
            );
            description.setName(&NSString::from_str(DEVICE_NAME));
            description.setPrivate(true);
            description
        };
        let tap_uid = unsafe { description.UUID().UUIDString() }.to_string();

        let mut tap: AudioObjectID = 0;
        let status = unsafe { create_tap(Retained::as_ptr(&description), &mut tap) };
        if status != 0 || tap == 0 {
            bail!(
                "Could not tap the system audio (error {status}); check the \"System Audio \
                 Recording\" permission in System Settings > Privacy & Security"
            );
        }

        let description = aggregate_description(&tap_uid, default_output_uid().as_deref());
        let mut aggregate: AudioObjectID = 0;
        let status = unsafe {
            AudioHardwareCreateAggregateDevice(
                &*(Retained::as_ptr(&description) as *const CFDictionary),
                NonNull::from(&mut aggregate),
            )
        };
        if status != 0 || aggregate == 0 {
            unsafe { destroy_tap(tap) };
            bail!("Could not create the system audio device (error {status})");
        }

        Ok(Self {
            tap,
            aggregate,
            destroy_tap,
        })
    }
}

impl Drop for SystemTap {
    fn drop(&mut self) {
        let status = unsafe { AudioHardwareDestroyAggregateDevice(self.aggregate) };
        if status != 0 {
            warn!("Failed to destroy the meeting audio device: {status}");
        }
        let status = unsafe { (self.destroy_tap)(self.tap) };
        if status != 0 {
            warn!("Failed to destroy the system audio tap: {status}");
        }
    }
}

/// `AudioHardwareCreateProcessTap` / `AudioHardwareDestroyProcessTap`, when the
/// running macOS has them.
fn tap_functions() -> Option<(CreateTapFn, DestroyTapFn)> {
    let create = unsafe {
        libc::dlsym(
            libc::RTLD_DEFAULT,
            c"AudioHardwareCreateProcessTap".as_ptr(),
        )
    };
    let destroy = unsafe {
        libc::dlsym(
            libc::RTLD_DEFAULT,
            c"AudioHardwareDestroyProcessTap".as_ptr(),
        )
    };
    if create.is_null() || destroy.is_null() {
        return None;
    }
    unsafe {
        Some((
            std::mem::transmute::<*mut c_void, CreateTapFn>(create),
            std::mem::transmute::<*mut c_void, DestroyTapFn>(destroy),
        ))
    }
}

/// A private aggregate device holding the tap, clocked by the output device
/// when its UID is known.
fn aggregate_description(
    tap_uid: &str,
    output_uid: Option<&str>,
) -> Retained<NSDictionary<NSString, AnyObject>> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let device_uid = NSString::from_str(&format!("handy-meeting-{}-{unique}", std::process::id()));
    let name = NSString::from_str(DEVICE_NAME);
    let yes = NSNumber::numberWithBool(true);
    let no = NSNumber::numberWithBool(false);
    let tap_uid = NSString::from_str(tap_uid);

    let tap = NSDictionary::<NSString, AnyObject>::from_slices(
        &[
            &*key(kAudioSubTapUIDKey),
            &*key(kAudioSubTapDriftCompensationKey),
        ],
        &[any(&tap_uid), any(&yes)],
    );
    let taps = NSArray::from_slice(&[&*tap]);

    let mut keys = vec![
        key(kAudioAggregateDeviceNameKey),
        key(kAudioAggregateDeviceUIDKey),
        key(kAudioAggregateDeviceIsPrivateKey),
        key(kAudioAggregateDeviceIsStackedKey),
        key(kAudioAggregateDeviceTapAutoStartKey),
        key(kAudioAggregateDeviceTapListKey),
    ];
    let mut values: Vec<&AnyObject> = vec![
        any(&name),
        any(&device_uid),
        any(&yes),
        any(&no),
        any(&yes),
        any(&taps),
    ];

    let output = output_uid.map(|uid| {
        let uid = NSString::from_str(uid);
        let sub_device = NSDictionary::<NSString, AnyObject>::from_slices(
            &[&*key(kAudioSubDeviceUIDKey)],
            &[any(&uid)],
        );
        let sub_devices = NSArray::from_slice(&[&*sub_device]);
        (uid, sub_devices)
    });
    if let Some((uid, sub_devices)) = &output {
        keys.push(key(kAudioAggregateDeviceMainSubDeviceKey));
        values.push(any(uid));
        keys.push(key(kAudioAggregateDeviceSubDeviceListKey));
        values.push(any(sub_devices));
    }

    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    NSDictionary::from_slices(&keys, &values)
}

fn key(name: &CStr) -> Retained<NSString> {
    NSString::from_str(name.to_str().expect("Core Audio keys are ASCII"))
}

/// Any Objective-C object as a dictionary value (deref coercion does the work).
fn any(object: &AnyObject) -> &AnyObject {
    object
}

/// UID of the default system output device.
fn default_output_uid() -> Option<String> {
    let mut device: AudioObjectID = 0;
    get_property(
        kAudioObjectSystemObject as AudioObjectID,
        kAudioHardwarePropertyDefaultSystemOutputDevice,
        &mut device,
    )?;
    let mut uid: *const CFString = ptr::null();
    get_property(device, kAudioDevicePropertyDeviceUID, &mut uid)?;
    // The caller owns the returned string.
    let uid = unsafe { CFRetained::from_raw(NonNull::new(uid as *mut CFString)?) };
    Some(uid.to_string())
}

fn get_property<T>(object: AudioObjectID, selector: u32, out: &mut T) -> Option<()> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size = std::mem::size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(out).cast(),
        )
    };
    (status == 0).then_some(())
}
