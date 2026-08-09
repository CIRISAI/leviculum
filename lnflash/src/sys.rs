//! Every `unsafe` block in the crate, in one file.
//!
//! The batch constraint is that nothing outside the bundle is needed: no
//! `stty`, no `udevadm`, no `mount(8)`, no `udisksctl`. What that costs is
//! three syscalls the standard library does not wrap — `tcsetattr`, `ioctl`
//! and `mount(2)` — so they live here behind owned handles, and no other
//! module writes `unsafe`.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

fn cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

fn last_error<T>(what: &str) -> io::Result<T> {
    let err = io::Error::last_os_error();
    Err(io::Error::new(err.kind(), format!("{what}: {err}")))
}

/// An owned file descriptor that closes itself.
#[derive(Debug)]
pub struct Fd(libc::c_int);

impl Fd {
    /// Open a serial port the way a flasher has to: no controlling terminal,
    /// and non-blocking so a port with nothing on the other end cannot hang
    /// the open on carrier detect.
    pub fn open_serial(path: &Path) -> io::Result<Self> {
        let c_path = cstring(path)?;
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return last_error(&format!("opening {}", path.display()));
        }
        Ok(Self(fd))
    }

    fn termios(&self) -> io::Result<libc::termios> {
        let mut tio: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(self.0, &mut tio) } != 0 {
            return last_error("tcgetattr");
        }
        Ok(tio)
    }

    fn set_termios(&self, tio: &libc::termios) -> io::Result<()> {
        if unsafe { libc::tcsetattr(self.0, libc::TCSANOW, tio) } != 0 {
            return last_error("tcsetattr");
        }
        Ok(())
    }

    /// The 1200-baud touch. The line-coding change is the whole message —
    /// the firmware resets on it — so nothing is written to the port.
    pub fn set_baud_1200(&self) -> io::Result<()> {
        let mut tio = self.termios()?;
        unsafe {
            libc::cfmakeraw(&mut tio);
            if libc::cfsetispeed(&mut tio, libc::B1200) != 0
                || libc::cfsetospeed(&mut tio, libc::B1200) != 0
            {
                return last_error("cfsetspeed");
            }
        }
        self.set_termios(&tio)
    }

    /// Raw 8N1 at 115200 with DTR and RTS asserted, which is how the debug
    /// port is read. **The debug CDC transmits only with DTR+RTS asserted;
    /// without them a healthy board reads as silent** — a false "no banner"
    /// would then be reported as an unverified flash.
    pub fn set_debug_port(&self) -> io::Result<()> {
        let mut tio = self.termios()?;
        unsafe {
            libc::cfmakeraw(&mut tio);
            tio.c_cflag |= libc::CLOCAL | libc::CREAD | libc::CS8;
            if libc::cfsetispeed(&mut tio, libc::B115200) != 0
                || libc::cfsetospeed(&mut tio, libc::B115200) != 0
            {
                return last_error("cfsetspeed");
            }
        }
        self.set_termios(&tio)?;
        let bits: libc::c_int = libc::TIOCM_DTR | libc::TIOCM_RTS;
        if unsafe { libc::ioctl(self.0, libc::TIOCMBIS, &bits) } != 0 {
            return last_error("ioctl(TIOCMBIS, DTR|RTS)");
        }
        Ok(())
    }

    /// Read whatever arrives until `deadline`, returning the bytes.
    ///
    /// Used to catch a banner the firmware re-emits every few seconds, so
    /// running out of time is an ordinary outcome, not an error.
    pub fn read_until(&self, deadline: Instant, max: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        while Instant::now() < deadline && out.len() < max {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !self.wait_readable(remaining)? {
                continue;
            }
            let n = unsafe { libc::read(self.0, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                out.extend_from_slice(&buf[..n as usize]);
            } else if n == 0 {
                break;
            } else {
                let err = io::Error::last_os_error();
                match err.kind() {
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => continue,
                    // The board rebooting mid-read is not a read failure.
                    _ => break,
                }
            }
        }
        Ok(out)
    }

    fn wait_readable(&self, timeout: Duration) -> io::Result<bool> {
        let mut pfd = libc::pollfd {
            fd: self.0,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        let ready = unsafe { libc::poll(&mut pfd, 1, millis) };
        if ready < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(err);
        }
        Ok(ready > 0)
    }
}

impl Drop for Fd {
    fn drop(&mut self) {
        // A close that fails has nothing left to report to: the descriptor
        // is gone either way, and on the touch path the port disappearing
        // underneath us is the intended outcome.
        unsafe { libc::close(self.0) };
    }
}

/// A filesystem mounted by us, unmounted when dropped.
///
/// `mount(2)` directly, because `mount(8)` and `udisksctl` are exactly the
/// external dependencies the bundle exists to avoid, and automounting
/// assumes a desktop stack a headless host does not have.
#[derive(Debug)]
pub struct Mount {
    target: std::path::PathBuf,
    active: bool,
}

impl Mount {
    /// Mount `device` at `target` as vfat. Needs root: the mass-storage
    /// device appears as `/dev/sdX` owned `root:disk`.
    pub fn vfat(device: &Path, target: &Path) -> io::Result<Self> {
        let c_device = cstring(device)?;
        let c_target = cstring(target)?;
        let fstype = CString::new("vfat").expect("no NUL in a literal");
        let rc = unsafe {
            libc::mount(
                c_device.as_ptr(),
                c_target.as_ptr(),
                fstype.as_ptr(),
                libc::MS_NOATIME | libc::MS_NODEV | libc::MS_NOEXEC | libc::MS_NOSUID,
                std::ptr::null(),
            )
        };
        if rc != 0 {
            let err = io::Error::last_os_error();
            let hint = if err.kind() == io::ErrorKind::PermissionDenied {
                " (the bootloader drive is root:disk, so this needs sudo)"
            } else {
                ""
            };
            return Err(io::Error::new(
                err.kind(),
                format!(
                    "mounting {} at {}{hint}: {err}",
                    device.display(),
                    target.display()
                ),
            ));
        }
        Ok(Self {
            target: target.to_path_buf(),
            active: true,
        })
    }

    pub fn path(&self) -> &Path {
        &self.target
    }

    /// Unmount, reporting failure. Dropping does the same thing silently;
    /// call this when the caller can act on the answer.
    pub fn unmount(mut self) -> io::Result<()> {
        self.unmount_inner()
    }

    fn unmount_inner(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let c_target = cstring(&self.target)?;
        if unsafe { libc::umount2(c_target.as_ptr(), 0) } == 0 {
            return Ok(());
        }
        let plain = io::Error::last_os_error();
        // The bootloader reboots on the last block written, so by the time
        // we unmount the device is often already gone. A lazy unmount then
        // detaches the stale mount rather than leaving it behind.
        if unsafe { libc::umount2(c_target.as_ptr(), libc::MNT_DETACH) } == 0 {
            return Ok(());
        }
        Err(io::Error::new(
            plain.kind(),
            format!("unmounting {}: {plain}", self.target.display()),
        ))
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let _ = self.unmount_inner();
    }
}

/// Whether the process can mount and write a `root:disk` block device.
pub fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_a_port_that_is_not_there_names_the_port() {
        let err = Fd::open_serial(Path::new("/dev/ttyDoesNotExist")).unwrap_err();
        assert!(format!("{err}").contains("/dev/ttyDoesNotExist"), "{err}");
    }

    #[test]
    fn a_path_with_a_nul_byte_is_rejected_before_it_reaches_a_syscall() {
        let bad =
            Path::new(unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(b"/dev/tty\0ACM0") });
        assert!(Fd::open_serial(bad).is_err());
    }

    #[test]
    fn mounting_without_privileges_says_so_rather_than_just_failing() {
        if is_root() {
            // The suite is not run as root; if it is, this check is vacuous.
            return;
        }
        let target = tempfile::tempdir().unwrap();
        let err = Mount::vfat(Path::new("/dev/null"), target.path()).unwrap_err();
        let text = format!("{err}");
        assert!(text.contains("/dev/null") && text.contains(&target.path().display().to_string()));
    }
}
