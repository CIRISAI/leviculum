//! A stock client must keep taking the announce of a node that serves
//! `/lev/status` (order 1394). The capability rides `PN_META_CUSTOM` 0xFF
//! in the announce's metadata map; the vendored Python reference validates
//! a propagation-node announce with `pn_announce_data_is_valid`
//! (`reference/LXMF/LXMF/LXMF.py:224-250`), which checks only that field 6
//! is a dict (`:244`). This feeds our exact app data to that function and
//! to `umsgpack`, so a change on either side that would make stock clients
//! drop the node is red here, not on the air.

use std::path::PathBuf;
use std::process::Command;

use leviculum_lxmf::{MemoryPropagationStore, PropagationNode, PropagationNodeConfig};

#[test]
fn the_reference_accepts_an_announce_that_carries_the_status_capability() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut node = PropagationNode::new(
        MemoryPropagationStore::new(64_000),
        PropagationNodeConfig {
            name: Some(b"lev-status".to_vec()),
            ..PropagationNodeConfig::default()
        },
    );
    node.serve_lev_status();
    let app_data = node.announce_app_data(1_762_000_000);
    let hex: String = app_data.iter().map(|b| format!("{b:02x}")).collect();

    let script = r#"
import sys
sys.path[:0] = [sys.argv[1] + "/reference/Reticulum", sys.argv[1] + "/reference/LXMF"]
import RNS.vendor.umsgpack as msgpack
from LXMF import LXMF
data = bytes.fromhex(sys.argv[2])
print(LXMF.pn_announce_data_is_valid(data))
print(LXMF.pn_name_from_app_data(data))
print(msgpack.unpackb(data)[6][LXMF.PN_META_CUSTOM])
"#;
    let output = Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(script)
        .arg(&root)
        .arg(&hex)
        .output()
        .expect("python3 runs");
    assert!(
        output.status.success(),
        "the reference failed on our announce:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec!["True", "lev-status", "{'lev': {'status': 1}}"],
        "valid, the name still read, the capability decoded as a dict"
    );
}
