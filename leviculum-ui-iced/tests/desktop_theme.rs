//! The desktop's tone on a host without a session bus (1310): the window
//! asks D-Bus for the freedesktop colour scheme, and a host without a bus
//! (a server, a container, a gate) must neither panic nor hang the window.
//! Its own test binary: it rewrites the process's bus environment.

#![cfg(all(target_os = "linux", not(target_env = "musl")))]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use iced::futures::executor::block_on;
use iced::futures::StreamExt;
use iced::theme::Mode;

#[test]
fn without_a_session_bus_the_tone_is_none_and_the_stream_ends() {
    // No bus: no address, and a runtime directory without a `bus` socket,
    // which is where zbus looks next.
    let runtime = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("desktop-theme-no-bus-{}", std::process::id()));
    std::fs::create_dir_all(&runtime).unwrap();
    std::env::remove_var("DBUS_SESSION_BUS_ADDRESS");
    std::env::set_var("XDG_RUNTIME_DIR", &runtime);

    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let modes: Vec<Mode> = block_on(leviculum_ui_iced::desktop::linux::stream().collect());
        let _ = tx.send(modes);
    });
    // Generous: the failed connect is a missing socket, answered at once;
    // a bound of seconds catches a hang, not a slow host.
    let modes = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the stream neither ended nor panicked within 5 s");
    eprintln!("no bus: {modes:?} in {:?}", started.elapsed());
    assert_eq!(modes, vec![Mode::None]);
    let _ = std::fs::remove_dir_all(&runtime);
}
