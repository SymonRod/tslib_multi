use jni::JNIEnv;

const EXCEPTION_CLASS: &str = "dev/tslib/TsLibException";

/// Throw a `TsLibException` on the Java side.
pub fn throw_tslib_exception(env: &mut JNIEnv, msg: &str) {
    let _ = env.throw_new(EXCEPTION_CLASS, msg);
}

/// Convert a `tslib_core::Result<T>` into an `Option<T>`, throwing a Java
/// exception on error.
pub fn to_jni_result<T>(env: &mut JNIEnv, result: tslib_core::Result<T>) -> Option<T> {
    match result {
        Ok(val) => Some(val),
        Err(err) => {
            throw_tslib_exception(env, &err.to_string());
            None
        }
    }
}

/// Convert a `tslib_audio::Result<T>` into an `Option<T>`, throwing a Java
/// exception on error.
pub fn audio_to_jni_result<T>(
    env: &mut JNIEnv,
    result: tslib_audio::Result<T>,
) -> Option<T> {
    match result {
        Ok(val) => Some(val),
        Err(err) => {
            throw_tslib_exception(env, &err.to_string());
            None
        }
    }
}

/// Run a JNI entry point, turning a Rust panic into a `TsLibException`.
///
/// A panic that reaches the FFI boundary aborts the whole process, which on
/// Android means the app vanishes with no stack trace and nothing the Java
/// side can catch. Every `Java_*` function funnels through here so that a bug
/// in the native layer surfaces as a normal exception instead.
///
/// `fallback` is the value returned to Java after a panic; it is only ever
/// observed by the caller if it ignores the pending exception.
pub fn guard<'local, T>(
    env: &mut JNIEnv<'local>,
    name: &str,
    fallback: T,
    f: impl FnOnce(&mut JNIEnv<'local>) -> T,
) -> T {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(env)));
    match result {
        Ok(val) => val,
        Err(payload) => {
            let msg = panic_message(&payload);
            log::error!("{}: native panic: {}", name, msg);
            throw_tslib_exception(env, &format!("native panic in {}: {}", name, msg));
            fallback
        }
    }
}

/// Best-effort extraction of the message carried by a panic payload.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}
