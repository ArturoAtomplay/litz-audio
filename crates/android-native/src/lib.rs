//! JNI entry points for the Android service. Kotlin owns and serializes native handles.
#![cfg_attr(
    not(target_os = "android"),
    allow(
        dead_code,
        reason = "only the Android JNI exports call into the engine"
    )
)]
#![warn(clippy::undocumented_unsafe_blocks)]
#[cfg(any(target_os = "android", target_os = "linux"))]
mod audio;
#[cfg(any(target_os = "android", target_os = "linux"))]
mod engine;
#[cfg(any(target_os = "android", target_os = "linux"))]
mod pairing;

use litz_core::transport::Result;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "android")]
use std::ffi::c_void;
use std::net::SocketAddr;

#[cfg(target_os = "android")]
const MAX_JNI_STRING_UNITS: usize = 64 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct Identity {
    desktop: Vec<u8>,
    certificate: Vec<u8>,
    private_key: Vec<u8>,
    endpoint: SocketAddr,
}
fn fresh_identity() -> Result<Identity> {
    let phone = rcgen::generate_simple_self_signed(vec!["litz.local".into()])?;
    Ok(Identity {
        desktop: Vec::new(),
        certificate: phone.cert.der().to_vec(),
        private_key: phone.signing_key.serialize_der(),
        endpoint: "0.0.0.0:47165".parse()?,
    })
}

#[cfg(target_os = "android")]
unsafe extern "C" {
    fn litz_jni_length(env: *mut c_void, text: *mut c_void) -> i32;
    fn litz_jni_exception(env: *mut c_void) -> i32;
    fn litz_jni_chars(env: *mut c_void, text: *mut c_void) -> *const u16;
    fn litz_jni_release(env: *mut c_void, text: *mut c_void, chars: *const u16);
    fn litz_jni_string(env: *mut c_void, text: *const u16, length: i32) -> *mut c_void;
}
// JNI contract: the JVM passes a live `env` and live references for the calling
// thread. Kotlin serializes calls on a handle and closes each handle once.
#[cfg(target_os = "android")]
/// # Safety
/// `env` and `value` must follow the JNI contract above.
unsafe fn text(env: *mut c_void, value: *mut c_void) -> Result<String> {
    if value.is_null() {
        return Err("JNI string unavailable".into());
    }
    // SAFETY: `value` is non-null and, per this function's contract, a live string.
    let length = unsafe { litz_jni_length(env, value) };
    if length < 0 || length as usize > MAX_JNI_STRING_UNITS {
        return Err("JNI string length invalid".into());
    }
    // SAFETY: `value` is live and non-null; the characters are released below.
    let chars = unsafe { litz_jni_chars(env, value) };
    if chars.is_null() {
        return Err("JNI string unavailable".into());
    }
    // SAFETY: JNI returns `length` UTF-16 units at `chars`.
    let units = unsafe { std::slice::from_raw_parts(chars, length as usize) };
    let result = String::from_utf16(units).map_err(|_| "invalid UTF-16".into());
    // SAFETY: released once, for the string it came from.
    unsafe {
        litz_jni_release(env, value, chars);
    }
    result
}
#[cfg(target_os = "android")]
/// # Safety
/// `env` must follow the JNI contract above.
unsafe fn reply(env: *mut c_void, value: Result<String>) -> *mut c_void {
    // SAFETY: `env` is the calling thread's environment, per this function's contract.
    if unsafe { litz_jni_exception(env) } != 0 {
        return std::ptr::null_mut();
    }
    let output = value.unwrap_or_else(|e| serde_json::json!({"error":e.to_string()}).to_string());
    let units: Vec<u16> = output.encode_utf16().collect();
    let Ok(length) = i32::try_from(units.len()) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `units` outlives the call; `length` fits a jsize.
    unsafe { litz_jni_string(env, units.as_ptr(), length) }
}
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
unsafe extern "system" fn Java_audio_litz_Native_createIdentity(
    env: *mut c_void,
    _: *mut c_void,
) -> *mut c_void {
    // SAFETY: `env` is the environment the JVM passed to this call.
    unsafe {
        reply(
            env,
            fresh_identity().and_then(|id| Ok(serde_json::to_string(&id)?)),
        )
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
unsafe extern "system" fn Java_audio_litz_Native_pairStart(
    env: *mut c_void,
    _: *mut c_void,
    identity: *mut c_void,
    endpoint: *mut c_void,
    expected: *mut c_void,
    name: *mut c_void,
) -> i64 {
    let result = (|| -> Result<pairing::Pairing> {
        // SAFETY: `env` and the four strings are live references passed to this call.
        let (identity, endpoint, expected, name) = unsafe {
            (
                text(env, identity)?,
                text(env, endpoint)?,
                text(env, expected)?,
                text(env, name)?,
            )
        };
        let identity: Identity = serde_json::from_str(&identity)?;
        let address: SocketAddr = endpoint.parse()?;
        pairing::Pairing::start(identity, address, expected, name)
    })();
    result
        .map(|pair| Box::into_raw(Box::new(pair)) as i64)
        .unwrap_or(0)
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
unsafe extern "system" fn Java_audio_litz_Native_pairStatus(
    env: *mut c_void,
    _: *mut c_void,
    handle: i64,
) -> *mut c_void {
    let value = if handle == 0 {
        Err("invalid pairing handle".into())
    } else {
        // SAFETY: Kotlin closes this handle only after its status loop ends.
        unsafe { &*(handle as *const pairing::Pairing) }.status()
    };
    // SAFETY: `env` is the environment the JVM passed to this call.
    unsafe { reply(env, value) }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
unsafe extern "system" fn Java_audio_litz_Native_pairCancel(
    _: *mut c_void,
    _: *mut c_void,
    handle: i64,
) {
    if handle != 0 {
        // SAFETY: Kotlin's pairLifetime lock keeps close from running concurrently.
        unsafe { &*(handle as *const pairing::Pairing) }.cancel();
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
unsafe extern "system" fn Java_audio_litz_Native_pairClose(
    _: *mut c_void,
    _: *mut c_void,
    handle: i64,
) {
    if handle != 0 {
        // SAFETY: the handle came from Box::into_raw in pairStart; Kotlin clears it before
        // this call, so it is freed exactly once.
        drop(unsafe { Box::from_raw(handle as *mut pairing::Pairing) });
    }
}
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
unsafe extern "system" fn Java_audio_litz_Native_open(
    env: *mut c_void,
    _: *mut c_void,
    material: *mut c_void,
) -> i64 {
    // SAFETY: `env` and `material` are live references passed to this call.
    let result = unsafe { text(env, material) }.and_then(|s| {
        if s.len() > 32768 {
            return Err("identity size".into());
        }
        engine::Engine::try_new(serde_json::from_str(&s)?)
    });
    result
        .map(|e| Box::into_raw(Box::new(e)) as i64)
        .unwrap_or(0)
}
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
unsafe extern "system" fn Java_audio_litz_Native_command(
    env: *mut c_void,
    _: *mut c_void,
    handle: i64,
    command: *mut c_void,
) -> *mut c_void {
    // SAFETY: `env` and `command` are live references passed to this call.
    let result = unsafe { text(env, command) }.and_then(|s| {
        if handle == 0 || s.len() > 4096 {
            return Err("invalid command/handle".into());
        }
        // SAFETY: the service serializes commands and closes the handle after the last one.
        unsafe { &*(handle as *const engine::Engine) }.command(&s)
    });
    // SAFETY: `env` is the environment the JVM passed to this call.
    unsafe { reply(env, result) }
}
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
unsafe extern "system" fn Java_audio_litz_Native_cancel(
    _: *mut c_void,
    _: *mut c_void,
    handle: i64,
) {
    if handle != 0 {
        // SAFETY: the service's lifetime lock keeps close from running concurrently.
        unsafe { &*(handle as *const engine::Engine) }.cancel();
    }
}
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
unsafe extern "system" fn Java_audio_litz_Native_close(
    _: *mut c_void,
    _: *mut c_void,
    handle: i64,
) {
    if handle != 0 {
        // SAFETY: the handle came from Box::into_raw in open; the service closes it
        // once, after its last native call.
        drop(unsafe { Box::from_raw(handle as *mut engine::Engine) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_identity_survives_restart_without_authorizing_a_desktop() {
        let id = fresh_identity().unwrap();
        let restored: Identity =
            serde_json::from_str(&serde_json::to_string(&id).unwrap()).unwrap();
        assert_eq!(id.private_key, restored.private_key);
        assert_eq!(id.certificate, restored.certificate);
        assert!(restored.desktop.is_empty());
    }
}
