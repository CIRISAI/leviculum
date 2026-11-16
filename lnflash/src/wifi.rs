//! `lnflash --wifi <SSID> [--wifi-pass <PASS>] --port <tty>` (1479): tell a
//! ThinkNode M9 which WLAN to join.
//!
//! The record (`leviculum_core::wifi_credentials`) goes into the board's
//! `wifi` partition at 0xE000 (`leviculum-esp/partitions-m9.csv`) through
//! the ROM loader, as the image and the map regions do; the board reads
//! it at boot, joins the network and runs its AutoInterface over it. A
//! board that never got one keeps its WiFi radio off. The passphrase is
//! not echoed: what lnflash says names the network only.

use std::path::Path;

use leviculum_core::wifi_credentials::{WifiCredentials, RECORD_LEN};

use crate::maps;

/// Where the record lives in the M9's flash.
pub const WIFI_OFFSET: u32 = 0xE000;

/// The sector as it is written: the record, the rest erased.
pub fn sector_of(ssid: &str, passphrase: &str) -> Result<Vec<u8>, String> {
    let record = WifiCredentials::new(ssid.as_bytes(), passphrase.as_bytes())
        .map_err(|why| why.to_string())?;
    let mut sector = vec![0xFF; 0x1000];
    sector[..RECORD_LEN].copy_from_slice(&record.encode());
    Ok(sector)
}

/// `--wifi`: write the network into the M9 on `port`. What to say.
pub fn write(port: &Path, ssid: &str, passphrase: &str) -> Result<String, String> {
    let sector = sector_of(ssid, passphrase)?;
    let dir = tempfile::tempdir().map_err(|e| format!("a scratch directory: {e}"))?;
    let path = dir.path().join("wifi.bin");
    std::fs::write(&path, &sector).map_err(|e| format!("{}: {e}", path.display()))?;
    maps::run(&maps::write_at_args(port, WIFI_OFFSET, &path))?;
    let how = if passphrase.is_empty() {
        "an open network"
    } else {
        "with its passphrase"
    };
    Ok(format!(
        "{}: the M9 joins the WLAN \"{ssid}\" ({how}); it resets and its [WIFI] line says how it goes",
        port.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_sector_holds_a_record_the_board_reads_back() {
        let sector = sector_of("Heimnetz", "correct horse").expect("fits");
        assert_eq!(sector.len(), 0x1000);
        let back = WifiCredentials::decode(&sector).expect("decodes");
        assert_eq!(back.ssid(), b"Heimnetz");
        assert_eq!(back.passphrase(), b"correct horse");
        assert!(sector[RECORD_LEN..].iter().all(|&b| b == 0xFF));
        assert!(sector_of("Heimnetz", "short").is_err());
        assert!(sector_of("", "").is_err());
    }

    /// espflash writes where the partition table puts the `wifi` entry.
    #[test]
    fn espflash_writes_where_the_table_puts_the_wifi_entry() {
        let args: Vec<String> = maps::write_at_args(
            Path::new("/dev/ttyUSB0"),
            WIFI_OFFSET,
            Path::new("/x/w.bin"),
        )
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
        assert_eq!(
            args.join(" "),
            "write-bin --chip esp32s3 --non-interactive --baud 230400 --port /dev/ttyUSB0 \
             0xe000 /x/w.bin"
        );
        let table = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-esp/partitions-m9.csv"),
        )
        .expect("the table");
        let row = table
            .lines()
            .find(|l| l.starts_with("wifi,"))
            .expect("a wifi row");
        let cells: Vec<&str> = row.split(',').map(str::trim).collect();
        assert_eq!(cells[3], format!("{WIFI_OFFSET:#x}"));
        assert_eq!(cells[4], "0x1000");
    }
}
