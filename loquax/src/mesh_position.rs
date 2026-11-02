//! Positions the mesh announces (1302): a node's `lev.position`
//! destination, announced beside its delivery destination, carries its
//! fix in the app data, so the map shows every node in range with no
//! mailbox and no message (`docs/drafts/mailbox-for-friends-study-2026-11.md`,
//! 4.3).
//!
//! The destination is SINGLE, of the node's identity, so the identity
//! hash the announce proves also names the node's delivery address
//! ([`delivery_of`]): the core merges an announced fix with the LXMF
//! telemetry positions of the same peer ([`crate::ui::UiEvent::MeshPosition`]).
//!
//! The app data is the board side's wire (order 1301). Until that lands
//! this module reads the plain layout below and nothing else; a version it
//! does not know (the sealed group variant of the study among them) is no
//! position, never a guess.
//!
//! ```text
//! version   u8      1
//! lat_e7    i32 BE  degrees north times 1e7
//! lon_e7    i32 BE  degrees east times 1e7
//! alt_m     i16 BE  metres above sea level
//! fix_time  u32 BE  unix seconds of the fix
//! speed     u16 BE  centimetres a second
//! reserved  u8      0
//! ```

use leviculum_core::Destination;
use leviculum_lxmf::telemetry::Location;

/// The app name of the destination.
pub const APP_NAME: &str = "lev";
/// Its one aspect: `lev.position`.
pub const ASPECT: &str = "position";
/// The layout this module reads.
pub const VERSION: u8 = 1;
/// The app data's length in that layout.
pub const LEN: usize = 18;

/// The name hash an announce of `lev.position` carries.
pub fn name_hash() -> [u8; 10] {
    Destination::compute_name_hash(APP_NAME, &[ASPECT])
}

/// Whether an announce's name hash is `lev.position`'s.
pub fn is_position(heard: &[u8]) -> bool {
    heard == name_hash()
}

/// The delivery address (`lxmf.delivery`) of the identity whose hash is
/// `identity`: the peer the core keys positions by.
pub fn delivery_of(identity: &[u8; 16]) -> [u8; 16] {
    let name = Destination::compute_name_hash(
        leviculum_lxmf::node::APP_NAME,
        &[leviculum_lxmf::node::DELIVERY_ASPECT],
    );
    *Destination::compute_destination_hash(&name, identity).as_bytes()
}

/// The position in an announce's app data, or none for a layout this
/// module does not read.
pub fn decode(app_data: &[u8]) -> Option<Location> {
    if app_data.len() < LEN || app_data[0] != VERSION {
        return None;
    }
    let i32_at = |at: usize| {
        i32::from_be_bytes([
            app_data[at],
            app_data[at + 1],
            app_data[at + 2],
            app_data[at + 3],
        ])
    };
    let lat_e7 = i32_at(1);
    let lon_e7 = i32_at(5);
    if !(-900_000_000..=900_000_000).contains(&lat_e7)
        || !(-1_800_000_000..=1_800_000_000).contains(&lon_e7)
    {
        return None;
    }
    let alt_m = i16::from_be_bytes([app_data[9], app_data[10]]);
    let fix_time = u32::from_be_bytes([app_data[11], app_data[12], app_data[13], app_data[14]]);
    let speed = u16::from_be_bytes([app_data[15], app_data[16]]);
    Some(Location {
        latitude_e6: lat_e7 / 10,
        longitude_e6: lon_e7 / 10,
        altitude_e2: i32::from(alt_m) * 100,
        speed_e2: u32::from(speed),
        bearing_e2: 0,
        accuracy_e2: 0,
        last_update: i64::from(fix_time),
    })
}

/// The app data of `location` in the layout [`decode`] reads: the fixture
/// the tests and the device walk announce with.
pub fn encode(location: &Location) -> [u8; LEN] {
    let mut out = [0u8; LEN];
    out[0] = VERSION;
    out[1..5].copy_from_slice(&location.latitude_e6.saturating_mul(10).to_be_bytes());
    out[5..9].copy_from_slice(&location.longitude_e6.saturating_mul(10).to_be_bytes());
    let alt_m = (location.altitude_e2 / 100).clamp(i32::from(i16::MIN), i32::from(i16::MAX));
    out[9..11].copy_from_slice(&(alt_m as i16).to_be_bytes());
    let fix_time = location.last_update.clamp(0, i64::from(u32::MAX)) as u32;
    out[11..15].copy_from_slice(&fix_time.to_be_bytes());
    let speed = location.speed_e2.min(u32::from(u16::MAX)) as u16;
    out[15..17].copy_from_slice(&speed.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn florence() -> Location {
        Location {
            latitude_e6: 43_773_100,
            longitude_e6: 11_256_000,
            altitude_e2: 5_000,
            speed_e2: 140,
            bearing_e2: 0,
            accuracy_e2: 0,
            last_update: 1_793_500_000,
        }
    }

    #[test]
    fn a_fixture_announce_decodes_to_its_fix() {
        let data = encode(&florence());
        assert_eq!(data.len(), LEN);
        assert_eq!(decode(&data), Some(florence()));
        // South and west.
        let mut south = florence();
        south.latitude_e6 = -33_868_800;
        south.longitude_e6 = -151_209_300;
        assert_eq!(decode(&encode(&south)), Some(south));
    }

    #[test]
    fn another_layout_is_no_position() {
        let mut data = encode(&florence());
        data[0] = 2;
        assert_eq!(decode(&data), None, "an unknown version");
        assert_eq!(decode(&data[..LEN - 1]), None, "too short");
        assert_eq!(decode(b"Anna"), None, "a name, as a delivery announce has");
        let mut far = encode(&florence());
        far[1..5].copy_from_slice(&950_000_000i32.to_be_bytes());
        assert_eq!(decode(&far), None, "north of the pole");
    }

    #[test]
    fn the_aspect_is_lev_position_and_the_peer_its_delivery_address() {
        assert_eq!(
            name_hash(),
            Destination::compute_name_hash("lev", &["position"])
        );
        assert!(is_position(&name_hash()));
        assert!(!is_position(&Destination::compute_name_hash(
            "lxmf",
            &["delivery"]
        )));
        let identity = [7u8; 16];
        let lxmf = Destination::compute_name_hash("lxmf", &["delivery"]);
        assert_eq!(
            delivery_of(&identity),
            *Destination::compute_destination_hash(&lxmf, &identity).as_bytes()
        );
    }
}
