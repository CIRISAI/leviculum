//! The phone's structured events in logcat (1655, walk 9, 48): no
//! `ROOMS_LINK`, `ROOMS_LIST` or `ROOMS_SUGGEST` line reached the phone's
//! log, so a walk could not read `answered=` or how a link stood. Each
//! `EVENT_NAME key=val … t=<ms>` line of loquax's targets goes to logcat
//! under the tag `loquax` at info level (`adb logcat -s loquax`), through
//! the event log's line hook (`leviculum_std::event_log`).
//!
//! liblog's one call is foreign, so the module allows `unsafe_code`, as
//! `bitmap` does for libjnigraphics.

#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, CString};

/// `ANDROID_LOG_INFO`.
const INFO: c_int = 4;

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

/// One line to logcat. A NUL inside it, which a rendered line never
/// holds, ends it there rather than losing it.
fn write(line: &str) {
    let text = line.split('\0').next().unwrap_or_default();
    let Ok(text) = CString::new(text) else {
        return;
    };
    // SAFETY: both pointers are to NUL-terminated strings that live
    // across the call; liblog copies them.
    unsafe {
        __android_log_write(INFO, c"loquax".as_ptr(), text.as_ptr());
    }
}

/// loquax's structured events to logcat, once a process; later calls do
/// nothing.
pub fn install() {
    let _ = leviculum_std::event_log::install_line_subscriber("loquax", write);
}
