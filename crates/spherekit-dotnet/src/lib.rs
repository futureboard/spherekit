//! Native C ABI for the SphereKit .NET wrapper.
//!
//! The managed package owns the convenient C# node builders and callbacks.
//! This crate owns the window, GPU surface and native host tree. Keeping this
//! boundary in C makes the wrapper usable from regular .NET JIT applications
//! without requiring a C++/CLI toolchain or a generated binding runtime.

#![deny(missing_docs)]

mod runner;

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};

use spherekit_react::ReactHost;

pub(crate) use runner::{EventCallback, FreeCallback, RenderCallback};

thread_local! {
    /// Error text is read immediately by the managed caller on the same UI
    /// thread, then replaced by the next exported call.
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

const NODE_TYPES: &[&str] = &[
    "view",
    "text",
    "button",
    "slider",
    "knob",
    "fader",
    "toggle",
    "checkbox",
    "progress",
    "separator",
    "panel",
    "scroll-view",
    "text-field",
    "avatar",
    "menu-item",
];

fn clear_error() {
    LAST_ERROR.with(|error| *error.borrow_mut() = None);
}

pub(crate) fn set_error(message: impl Into<String>) {
    let message = message.into().replace('\0', " ");
    LAST_ERROR.with(|error| {
        *error.borrow_mut() = Some(CString::new(message).expect("NUL was removed"));
    });
}

fn utf8_argument(pointer: *const c_char, name: &str) -> Result<String, String> {
    if pointer.is_null() {
        return Err(format!("{name} must not be null"));
    }
    // The C# side supplies NUL-terminated UTF-8 allocated for the duration of
    // the call. The pointer is only read and never retained.
    let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
    String::from_utf8(bytes.to_vec()).map_err(|_| format!("{name} must be valid UTF-8"))
}

/// Returns the version compiled into the native library.
#[unsafe(no_mangle)]
pub extern "C" fn spherekit_dotnet_version() -> *const c_char {
    static VERSION: std::sync::OnceLock<CString> = std::sync::OnceLock::new();
    VERSION
        .get_or_init(|| {
            CString::new(env!("CARGO_PKG_VERSION")).expect("package version has no NUL")
        })
        .as_ptr()
}

/// Returns the host node types as a JSON array.
#[unsafe(no_mangle)]
pub extern "C" fn spherekit_dotnet_node_types() -> *const c_char {
    static TYPES: std::sync::OnceLock<CString> = std::sync::OnceLock::new();
    TYPES
        .get_or_init(|| {
            CString::new(serde_json::to_string(NODE_TYPES).expect("node types encode"))
                .expect("node types have no NUL")
        })
        .as_ptr()
}

/// Returns the last error raised by an exported function on this thread.
///
/// The returned pointer remains valid until the next exported function call on
/// the same thread. The managed wrapper copies it immediately.
#[unsafe(no_mangle)]
pub extern "C" fn spherekit_dotnet_last_error() -> *const c_char {
    LAST_ERROR
        .with(|error| error.borrow().as_ref().map_or(std::ptr::null(), |value| value.as_ptr()))
}

/// Validates a complete native tree and returns its node count, or `-1`.
#[unsafe(no_mangle)]
pub extern "C" fn spherekit_dotnet_check_tree(json: *const c_char) -> i64 {
    clear_error();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let json = utf8_argument(json, "json")?;
        let mut host = ReactHost::new();
        host.commit_json(&json).map(|()| host.node_count()).map_err(|error| error.to_string())
    }));
    match result {
        Ok(Ok(count)) => count as i64,
        Ok(Err(error)) => {
            set_error(error);
            -1
        }
        Err(_) => {
            set_error("native tree validation panicked");
            -1
        }
    }
}

/// Parses a stylesheet and returns its rule count, or `-1`.
#[unsafe(no_mangle)]
pub extern "C" fn spherekit_dotnet_check_css(css: *const c_char) -> i64 {
    clear_error();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let css = utf8_argument(css, "css")?;
        spherekit_css::Stylesheet::parse(&css)
            .map(|sheet| sheet.rule_count())
            .map_err(|error| error.to_string())
    }));
    match result {
        Ok(Ok(count)) => count as i64,
        Ok(Err(error)) => {
            set_error(error);
            -1
        }
        Err(_) => {
            set_error("native stylesheet validation panicked");
            -1
        }
    }
}

/// Requests the active SphereKit window to close on the next event-loop turn.
#[unsafe(no_mangle)]
pub extern "C" fn spherekit_dotnet_quit() {
    runner::request_quit();
}

/// Runs a SphereKit window driven by managed render and event callbacks.
///
/// `frame_limit` is used only when `has_frame_limit` is non-zero. The callback
/// returned by `render` must return an allocated UTF-8 string; `free_string`
/// is called immediately after the native side has copied it. The C# wrapper
/// uses `CoTaskMemAlloc`/`FreeCoTaskMem` for this ownership hand-off.
///
/// Returns zero on success and `-1` on failure. Call
/// [`spherekit_dotnet_last_error`] immediately for diagnostics.
///
/// # Safety
///
/// All string pointers must be valid NUL-terminated UTF-8 for the duration of
/// the call. The callback pointers must remain valid until the function
/// returns, and `out_frames`, when non-null, must be writable for one `u64`.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn spherekit_dotnet_run(
    title: *const c_char,
    width: f32,
    height: f32,
    css: *const c_char,
    dark: u8,
    frame_limit: u64,
    has_frame_limit: u8,
    render: Option<RenderCallback>,
    on_event: Option<EventCallback>,
    free_string: Option<FreeCallback>,
    user_data: *mut std::ffi::c_void,
    out_frames: *mut u64,
) -> i32 {
    clear_error();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let config = runner::RunConfig {
            title: utf8_argument(title, "title")?,
            width,
            height,
            css: utf8_argument(css, "css")?,
            dark: dark != 0,
            frame_limit: (has_frame_limit != 0).then_some(frame_limit),
        };
        let render = render.ok_or_else(|| "render callback must not be null".to_owned())?;
        let on_event = on_event.ok_or_else(|| "event callback must not be null".to_owned())?;
        let free_string =
            free_string.ok_or_else(|| "free-string callback must not be null".to_owned())?;
        let frames = runner::run_loop(config, render, on_event, free_string, user_data)?;
        if !out_frames.is_null() {
            unsafe { *out_frames = frames };
        }
        Ok::<(), String>(())
    }));

    match result {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            set_error(error);
            -1
        }
        Err(_) => {
            set_error("native SphereKit runner panicked");
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    #[test]
    fn exports_the_same_widget_types_the_host_lowers() {
        let json = unsafe { CStr::from_ptr(spherekit_dotnet_node_types()) }
            .to_str()
            .expect("node type JSON is UTF-8");
        let types: Vec<&str> = serde_json::from_str(json).expect("node type JSON parses");
        assert!(types.contains(&"menu-item"));
        assert!(!types.contains(&"not-a-widget"));
    }

    #[test]
    fn invalid_tree_reports_an_error_without_crossing_ffi() {
        let json = CString::new("{not json").expect("test string");
        assert_eq!(spherekit_dotnet_check_tree(json.as_ptr()), -1);
        assert!(!spherekit_dotnet_last_error().is_null());
    }

    #[test]
    fn null_arguments_are_reported() {
        assert_eq!(spherekit_dotnet_check_css(ptr::null()), -1);
        let error = unsafe { CStr::from_ptr(spherekit_dotnet_last_error()) };
        assert!(error.to_str().expect("UTF-8").contains("css"));
    }
}
