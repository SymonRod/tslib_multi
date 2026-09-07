#![allow(non_snake_case)]

mod audio;
mod bbcode;
mod client;
mod error;
mod identity;
mod types;

use jni::objects::{JClass, JObject};
use jni::sys::jint;
use jni::JNIEnv;

/// Called when the native library is loaded via `System.loadLibrary`.
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(_vm: jni::JavaVM, _reserved: *mut std::ffi::c_void) -> jint {
    // Initialize Android logger so Rust log macros output to logcat
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Trace)
            .with_tag("tslib-jni"),
    );

    // Bridge tracing (used by tslib-core) → log (used by android_logger)
    // This makes all tracing::info!() etc. from the core library visible in logcat
    let _ = tracing_log::LogTracer::init();

    // Log Rust panics to logcat (otherwise they only reach stderr, which is
    // discarded on Android, leaving an unexplained SIGABRT)
    std::panic::set_hook(Box::new(|info| {
        log::error!("RUST PANIC: {}", info);
    }));

    log::info!("tslib-jni native library loaded (with tracing bridge)");
    jni::sys::JNI_VERSION_1_6
}

/// `Client.initAndroidContext(Context)` — publishes the `JavaVM` and the
/// application `Context` to `ndk_context`.
///
/// Crates such as `hickory-resolver` (DNS resolution used while connecting) and
/// `oboe`/`cpal` (audio) read the Android context from that global. It is
/// normally set up by `ndk-glue` for native-activity apps; in a plain JNI
/// library we have to do it ourselves, otherwise those crates panic with
/// "android context was not initialized" and the process aborts.
///
/// Safe to call more than once — only the first call takes effect.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeInitAndroidContext(
    env: JNIEnv,
    _class: JClass,
    context: JObject,
) {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let vm = match env.get_java_vm() {
            Ok(vm) => vm,
            Err(e) => {
                log::error!("initAndroidContext: cannot get JavaVM: {}", e);
                return;
            }
        };
        // The context reference must stay alive for the whole process lifetime,
        // so keep a global ref and never drop it.
        let ctx = match env.new_global_ref(context) {
            Ok(ctx) => ctx,
            Err(e) => {
                log::error!("initAndroidContext: cannot pin Context: {}", e);
                return;
            }
        };
        let vm_ptr = vm.get_java_vm_pointer().cast::<std::ffi::c_void>();
        let ctx_ptr = ctx.as_raw().cast::<std::ffi::c_void>();
        std::mem::forget(ctx);
        unsafe { ndk_context::initialize_android_context(vm_ptr, ctx_ptr) };
        log::info!("initAndroidContext: android context initialized");
    });
}

/// Helper to get a `String` from a `JString`, returning `None` on null.
pub(crate) fn get_string(env: &mut JNIEnv, s: &jni::objects::JString) -> Option<String> {
    if s.is_null() {
        return None;
    }
    env.get_string(s).ok().map(|s| s.into())
}

/// Helper to get a required `String` from a `JString`.
/// Throws `TsLibException` if null.
pub(crate) fn require_string(env: &mut JNIEnv, s: &jni::objects::JString) -> Result<String, ()> {
    match get_string(env, s) {
        Some(s) => Ok(s),
        None => {
            error::throw_tslib_exception(env, "String argument must not be null");
            Err(())
        }
    }
}
