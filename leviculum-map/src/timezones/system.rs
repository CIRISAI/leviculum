//! The system's tzdata (feature `std`): Linux's `/usr/share/zoneinfo`,
//! Android's packed file ([`super::packed`]).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::vec;
use std::vec::Vec;

use super::packed::{packed_find, packed_header, PACKED_HEADER};

/// Where Android keeps its packed tzdata, the updatable APEX first.
pub const ANDROID_TZDATA: [&str; 2] = [
    "/apex/com.android.tzdata/etc/tz/tzdata",
    "/system/usr/share/zoneinfo/tzdata",
];

/// Where every other system keeps its zone files.
pub const ZONEINFO: &str = "/usr/share/zoneinfo";

/// A zone name that may be looked up: not empty, not absolute, no `..`,
/// no NUL, no backslash.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains("..")
        && !name.contains('\0')
        && !name.contains('\\')
}

/// The TZif bytes of zone `name` in the system's tzdata: on Android from
/// the packed file, elsewhere `/usr/share/zoneinfo/<name>`. `None` for a
/// name [`valid_name`] refuses, a zone the system does not have, or a
/// file that is not TZif. Reads the file each call; a frontend keeps the
/// bytes of the zones it shows.
pub fn system_zone_bytes(name: &str) -> Option<Vec<u8>> {
    if !valid_name(name) {
        return None;
    }
    #[cfg(target_os = "android")]
    {
        ANDROID_TZDATA
            .iter()
            .find_map(|p| packed_zone_bytes(Path::new(p), name))
    }
    #[cfg(not(target_os = "android"))]
    {
        zoneinfo_bytes(Path::new(ZONEINFO), name)
    }
}

/// The TZif bytes of `name` under the zoneinfo directory `dir`.
pub fn zoneinfo_bytes(dir: &Path, name: &str) -> Option<Vec<u8>> {
    if !valid_name(name) {
        return None;
    }
    let bytes = std::fs::read(dir.join(name)).ok()?;
    bytes.starts_with(b"TZif").then_some(bytes)
}

/// The TZif bytes of `name` in the packed tzdata file at `path`, read by
/// seeking to the index and the zone, not the whole file.
pub fn packed_zone_bytes(path: &Path, name: &str) -> Option<Vec<u8>> {
    if !valid_name(name) {
        return None;
    }
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; PACKED_HEADER];
    f.read_exact(&mut head).ok()?;
    let (index, data) = packed_header(&head)?;
    let mut entries = vec![0u8; data - index];
    f.seek(SeekFrom::Start(index as u64)).ok()?;
    f.read_exact(&mut entries).ok()?;
    let (start, len) = packed_find(&entries, name)?;
    let mut zone = vec![0u8; len];
    f.seek(SeekFrom::Start(data.checked_add(start)? as u64))
        .ok()?;
    f.read_exact(&mut zone).ok()?;
    zone.starts_with(b"TZif").then_some(zone)
}
