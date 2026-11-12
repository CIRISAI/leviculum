//! `lnflash --region <file.lmr> --port <tty>` and `lnflash --regions
//! --port <tty>` (1477): put map regions onto a ThinkNode M9, and say which
//! it carries.
//!
//! The M9 keeps the regions in a partition of their own, `regions` at
//! 0xC12000, 4,116,480 B (`leviculum-esp/partitions-m9.csv`), as a
//! `leviculum_map::area`: a directory in the first sector, the regions
//! after it at sector boundaries. The board reads it at boot into its PSRAM
//! and names what it found in its `[MAP] regions=` line; where a region
//! lies under the view it wins over the world overview the image carries.
//!
//! The area is built here, every file checked to be a region first, and
//! written whole through the ESP32's ROM loader by espflash, the program
//! the M9's image is flashed with (`just flash-m9`); lnflash has no ROM
//! loader of its own since b737a11b. The write resets the board into the
//! loader and back, so the board boots with its new regions. Several
//! `--region` files go in one area; an area written replaces the one
//! before.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use leviculum_map::area::{self, Entry};
use leviculum_map::region::Region;

/// Where the area starts in the M9's flash.
pub const AREA_OFFSET: u32 = 0xC1_2000;

/// Its size: up to the client record's second slot at 0xFFF000.
pub const AREA_CAPACITY: usize = 0xFF_F000 - 0xC1_2000;

/// The rate the M9's CH340K holds for the ROM loader (`just flash-m9`).
pub const LOADER_BAUD: u32 = 230_400;

/// The area of the region files `files`, each named by its file stem cut
/// to the directory's 20 bytes. Refused before any board is touched: a
/// file that is no region, two of one name, regions past the area.
pub fn area_of(files: &[PathBuf]) -> Result<Vec<u8>, String> {
    let mut read: Vec<(String, Vec<u8>)> = Vec::new();
    for file in files {
        let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
        Region::from_bytes(&bytes)
            .map_err(|e| format!("{}: not a map region ({e:?})", file.display()))?;
        let name = name_of(file);
        if read.iter().any(|(n, _)| *n == name) {
            return Err(format!("two regions named {name}"));
        }
        read.push((name, bytes));
    }
    let regions: Vec<(&str, &[u8])> = read
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    area::build(&regions, AREA_CAPACITY).map_err(|e| match e {
        area::AreaError::Full { need, capacity } => {
            format!("the regions need {need} bytes, the M9's map area holds {capacity}")
        }
        other => format!("the regions do not make an area: {other}"),
    })
}

/// A region's name on the board: its file's stem, at most the
/// directory's 20 bytes, cut at a character boundary.
pub fn name_of(file: &Path) -> String {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("region"));
    let mut end = stem.len().min(area::NAME);
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    match end {
        0 => String::from("region"),
        _ => String::from(&stem[..end]),
    }
}

/// The regions the area's first sector names, in the words of a list.
pub fn listed(sector: &[u8]) -> Result<Vec<Entry>, String> {
    match area::directory(sector, AREA_CAPACITY) {
        Ok(entries) => Ok(entries),
        Err(area::AreaError::Empty) => Ok(Vec::new()),
        Err(e) => Err(format!("the M9's map area is not readable: {e}")),
    }
}

/// The lines `--regions` prints for `entries`.
pub fn said(port: &Path, entries: &[Entry]) -> String {
    let mut out = format!(
        "{}: the M9 carries the world overview and Florence in its image",
        port.display()
    );
    match entries.is_empty() {
        true => out.push_str(", and no region in its map area"),
        false => {
            out.push_str(", and in its map area:");
            for e in entries {
                out.push_str(&format!("\n  {} ({} bytes)", e.name, e.len));
            }
        }
    }
    out
}

/// espflash: on the PATH, else in cargo's bin, where `cargo install`
/// puts it.
pub fn espflash() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("espflash"))
            .find(|p| p.is_file())
    });
    on_path.or_else(|| {
        let home = std::env::var_os("HOME")?;
        let p = PathBuf::from(home).join(".cargo/bin/espflash");
        p.is_file().then_some(p)
    })
}

/// The arguments that write `image` at the area's start on `port`.
pub fn write_args(port: &Path, image: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "write-bin",
        "--chip",
        "esp32s3",
        "--non-interactive",
        "--baud",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    args.push(LOADER_BAUD.to_string().into());
    args.push("--port".into());
    args.push(port.into());
    args.push(format!("{AREA_OFFSET:#x}").into());
    args.push(image.into());
    args
}

/// The arguments that read the area's directory sector from `port` into
/// `out`.
pub fn read_args(port: &Path, out: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "read-flash",
        "--chip",
        "esp32s3",
        "--non-interactive",
        "--baud",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    args.push(LOADER_BAUD.to_string().into());
    args.push("--port".into());
    args.push(port.into());
    args.push(format!("{AREA_OFFSET:#x}").into());
    args.push(format!("{:#x}", area::SECTOR).into());
    args.push(out.into());
    args
}

/// Run espflash with `args`; an error in words when it is missing or
/// fails.
fn run(args: &[OsString]) -> Result<(), String> {
    let program = espflash().ok_or_else(|| {
        String::from("espflash is not installed: `cargo install espflash` puts it in ~/.cargo/bin")
    })?;
    let status = std::process::Command::new(&program)
        .args(args)
        .status()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    match status.success() {
        true => Ok(()),
        false => Err(format!("espflash failed ({status})")),
    }
}

/// `--region`: build the area of `files` and write it to the M9 on
/// `port`. What to say.
pub fn write(port: &Path, files: &[PathBuf]) -> Result<String, String> {
    let image = area_of(files)?;
    let dir = tempfile::tempdir().map_err(|e| format!("a scratch directory: {e}"))?;
    let path = dir.path().join("regions.bin");
    std::fs::write(&path, &image).map_err(|e| format!("{}: {e}", path.display()))?;
    run(&write_args(port, &path))?;
    let names: Vec<String> = files.iter().map(|f| name_of(f)).collect();
    Ok(format!(
        "{}: wrote {} ({} bytes) into the M9's map area; the board boots with them",
        port.display(),
        names.join(", "),
        image.len()
    ))
}

/// `--regions`: read the directory from the M9 on `port`. What to say.
pub fn list(port: &Path) -> Result<String, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("a scratch directory: {e}"))?;
    let path = dir.path().join("directory.bin");
    run(&read_args(port, &path))?;
    let sector = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(said(port, &listed(&sector)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(rel: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(rel)
    }

    /// The area of two real region files, read back as the board reads
    /// it: the directory names both, each slice at its entry is the file's
    /// bytes, passes its sum and opens as a region; the image fits the
    /// area. The control: a file that is no region is refused before
    /// anything is built, as is a name given twice.
    #[test]
    fn the_flash_image_holds_the_regions_the_board_reads_back() {
        let firenze = fixture("leviculum-map/fixtures/firenze.lmr");
        let roemer = fixture("leviculum-map/fixtures/frankfurt/roemer.lmr");
        let image = area_of(&[firenze.clone(), roemer.clone()]).expect("an area");
        assert!(image.len() <= AREA_CAPACITY);
        let entries = listed(&image[..area::SECTOR]).expect("a directory");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["firenze", "roemer"]);
        for (entry, file) in entries.iter().zip([&firenze, &roemer]) {
            let at = entry.offset as usize;
            let bytes = &image[at..at + entry.len as usize];
            assert_eq!(bytes, std::fs::read(file).expect("read"));
            assert_eq!(area::check(entry, bytes), Ok(()));
            assert!(Region::from_bytes(bytes).is_ok());
        }
        let said = said(Path::new("/dev/ttyUSB0"), &entries);
        assert!(said.contains("firenze (210647 bytes)"), "{said}");

        let readme = fixture("leviculum-map/fixtures/README.md");
        let refused = area_of(&[readme]).expect_err("no region");
        assert!(refused.contains("not a map region"), "{refused}");
        let twice = area_of(&[firenze.clone(), firenze]).expect_err("twice");
        assert!(twice.contains("two regions named firenze"), "{twice}");
        // An erased area lists nothing.
        assert!(listed(&[0xFF; area::SECTOR]).expect("erased").is_empty());
    }

    /// The commands write and read where the partition table puts the
    /// area, at the loader's rate the M9 is flashed at.
    #[test]
    fn espflash_writes_and_reads_the_area_where_the_table_puts_it() {
        let port = Path::new("/dev/ttyUSB0");
        let write: Vec<String> = write_args(port, Path::new("/x/regions.bin"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            write.join(" "),
            "write-bin --chip esp32s3 --non-interactive --baud 230400 --port /dev/ttyUSB0 \
             0xc12000 /x/regions.bin"
        );
        let read: Vec<String> = read_args(port, Path::new("/x/dir.bin"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            read.join(" "),
            "read-flash --chip esp32s3 --non-interactive --baud 230400 --port /dev/ttyUSB0 \
             0xc12000 0x1000 /x/dir.bin"
        );
        // The partition table says the same.
        let table =
            std::fs::read_to_string(fixture("leviculum-esp/partitions-m9.csv")).expect("the table");
        let row = table
            .lines()
            .find(|l| l.starts_with("regions,"))
            .expect("a regions row");
        let cells: Vec<&str> = row.split(',').map(str::trim).collect();
        assert_eq!(cells[3], format!("{AREA_OFFSET:#x}"));
        assert_eq!(
            usize::from_str_radix(cells[4].trim_start_matches("0x"), 16).expect("hex"),
            AREA_CAPACITY
        );
    }
}
