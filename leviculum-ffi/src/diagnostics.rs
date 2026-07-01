//! Read-only diagnostics, so a C program can build an `rnstatus`-style view of
//! a node: the transport counters and the path-table size.

use std::os::raw::c_int;

use leviculum_std::{InterfaceStatusSnapshot, PathTableExport};

use crate::error::*;
use crate::node::leviculum_t;
use crate::{guard, write_out, LEV_ADDR_LEN};

/// Read the transport counters into the provided out-parameters: packets sent,
/// received, forwarded, announces processed, packets dropped, and the current
/// path-table size. Any out-pointer may be NULL to skip that counter. Returns
/// `LEV_OK`, or `LEV_ERR_NULL_PTR` if `node` is NULL.
#[no_mangle]
pub unsafe extern "C" fn lev_transport_stats(
    node: *const leviculum_t,
    out_packets_sent: *mut u64,
    out_packets_received: *mut u64,
    out_packets_forwarded: *mut u64,
    out_announces_processed: *mut u64,
    out_packets_dropped: *mut u64,
    out_path_count: *mut u64,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let h = match node.as_ref() {
            Some(h) => h,
            None => return LEV_ERR_NULL_PTR,
        };
        let stats = h.node().transport_stats();
        if !out_packets_sent.is_null() {
            *out_packets_sent = stats.packets_sent();
        }
        if !out_packets_received.is_null() {
            *out_packets_received = stats.packets_received();
        }
        if !out_packets_forwarded.is_null() {
            *out_packets_forwarded = stats.packets_forwarded();
        }
        if !out_announces_processed.is_null() {
            *out_announces_processed = stats.announces_processed();
        }
        if !out_packets_dropped.is_null() {
            *out_packets_dropped = stats.packets_dropped();
        }
        if !out_path_count.is_null() {
            *out_path_count = h.node().path_count() as u64;
        }
        LEV_OK
    })
}

/// An owned, point-in-time snapshot of the path table. Take it with
/// `lev_path_table_snapshot`, read entries by index, and release it with
/// `lev_path_table_free`. Because it is a frozen copy, reads never race with a
/// changing table.
pub struct lev_path_table_t {
    entries: Vec<PathTableExport>,
}

/// Capture a snapshot of the node's path table. Returns an owned handle (free
/// with `lev_path_table_free`), or NULL on a NULL node.
#[no_mangle]
pub unsafe extern "C" fn lev_path_table_snapshot(
    node: *const leviculum_t,
) -> *mut lev_path_table_t {
    guard(std::ptr::null_mut(), || {
        let h = match node.as_ref() {
            Some(h) => h,
            None => return std::ptr::null_mut(),
        };
        let entries = h.node().path_table();
        Box::into_raw(Box::new(lev_path_table_t { entries }))
    })
}

/// Number of entries in a path-table snapshot, or 0 on NULL.
#[no_mangle]
pub unsafe extern "C" fn lev_path_table_count(table: *const lev_path_table_t) -> c_int {
    guard(0, || match table.as_ref() {
        Some(t) => t.entries.len() as c_int,
        None => 0,
    })
}

/// Read one entry of a path-table snapshot by index into the out-parameters.
/// `dest_hash` and `next_hop`, if non-NULL, must be at least `LEV_ADDR_LEN`
/// (16) bytes; `has_next_hop` reports whether `next_hop` was written (a relayed
/// path has one, a direct path does not). Any out-pointer may be NULL to skip
/// it. `LEV_ERR_INVALID_ARG` if `index` is out of range.
#[no_mangle]
pub unsafe extern "C" fn lev_path_table_entry(
    table: *const lev_path_table_t,
    index: usize,
    dest_hash: *mut u8,
    hops: *mut u8,
    next_hop: *mut u8,
    has_next_hop: *mut c_int,
    interface_index: *mut u64,
    expires_ms: *mut u64,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let t = match table.as_ref() {
            Some(t) => t,
            None => return LEV_ERR_NULL_PTR,
        };
        let entry = match t.entries.get(index) {
            Some(e) => e,
            None => {
                set_last_error("path entry index out of range");
                return LEV_ERR_INVALID_ARG;
            }
        };
        if !dest_hash.is_null() {
            std::ptr::copy_nonoverlapping(entry.hash.as_ptr(), dest_hash, LEV_ADDR_LEN);
        }
        if !hops.is_null() {
            *hops = entry.hops;
        }
        if !has_next_hop.is_null() {
            *has_next_hop = c_int::from(entry.next_hop.is_some());
        }
        if let Some(nh) = entry.next_hop.as_ref() {
            if !next_hop.is_null() {
                std::ptr::copy_nonoverlapping(nh.as_ptr(), next_hop, LEV_ADDR_LEN);
            }
        }
        if !interface_index.is_null() {
            *interface_index = entry.interface_index as u64;
        }
        if !expires_ms.is_null() {
            *expires_ms = entry.expires_ms;
        }
        LEV_OK
    })
}

/// Release a path-table snapshot. `lev_path_table_free(NULL)` is a no-op.
#[no_mangle]
pub unsafe extern "C" fn lev_path_table_free(table: *mut lev_path_table_t) {
    guard((), || {
        if !table.is_null() {
            drop(Box::from_raw(table));
        }
    })
}

/// An owned, point-in-time snapshot of every interface. Take it with
/// `lev_interface_stats_snapshot`, read entries by index, and release it with
/// `lev_interface_stats_free`.
pub struct lev_interface_stats_t {
    entries: Vec<InterfaceStatusSnapshot>,
}

/// Capture a snapshot of the node's interfaces (name, online status, byte
/// counters). Returns an owned handle (free with `lev_interface_stats_free`),
/// or NULL on a NULL node.
#[no_mangle]
pub unsafe extern "C" fn lev_interface_stats_snapshot(
    node: *const leviculum_t,
) -> *mut lev_interface_stats_t {
    guard(std::ptr::null_mut(), || {
        let h = match node.as_ref() {
            Some(h) => h,
            None => return std::ptr::null_mut(),
        };
        let entries = h.node().interface_stats();
        Box::into_raw(Box::new(lev_interface_stats_t { entries }))
    })
}

/// Number of interfaces in a snapshot, or 0 on NULL.
#[no_mangle]
pub unsafe extern "C" fn lev_interface_stats_count(table: *const lev_interface_stats_t) -> c_int {
    guard(0, || match table.as_ref() {
        Some(t) => t.entries.len() as c_int,
        None => 0,
    })
}

/// Write the name of interface `index` into `buf`, read(2) style (the name is
/// variable length; a NULL `buf` queries the length). `LEV_ERR_INVALID_ARG` if
/// `index` is out of range.
#[no_mangle]
pub unsafe extern "C" fn lev_interface_stats_name(
    table: *const lev_interface_stats_t,
    index: usize,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let t = match table.as_ref() {
            Some(t) => t,
            None => return LEV_ERR_NULL_PTR,
        };
        match t.entries.get(index) {
            Some(e) => write_out(e.name.as_bytes(), buf, cap, out_len),
            None => {
                set_last_error("interface index out of range");
                LEV_ERR_INVALID_ARG
            }
        }
    })
}

/// Read the scalar fields of interface `index` into the out-parameters: online
/// (1/0), is_local_client (1/0), and the byte counters. Any out-pointer may be
/// NULL to skip it. `LEV_ERR_INVALID_ARG` if `index` is out of range. Read the
/// name with `lev_interface_stats_name`.
#[no_mangle]
pub unsafe extern "C" fn lev_interface_stats_entry(
    table: *const lev_interface_stats_t,
    index: usize,
    online: *mut c_int,
    is_local_client: *mut c_int,
    rx_bytes: *mut u64,
    tx_bytes: *mut u64,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let t = match table.as_ref() {
            Some(t) => t,
            None => return LEV_ERR_NULL_PTR,
        };
        let entry = match t.entries.get(index) {
            Some(e) => e,
            None => {
                set_last_error("interface index out of range");
                return LEV_ERR_INVALID_ARG;
            }
        };
        if !online.is_null() {
            *online = c_int::from(entry.online);
        }
        if !is_local_client.is_null() {
            *is_local_client = c_int::from(entry.is_local_client);
        }
        if !rx_bytes.is_null() {
            *rx_bytes = entry.rx_bytes;
        }
        if !tx_bytes.is_null() {
            *tx_bytes = entry.tx_bytes;
        }
        LEV_OK
    })
}

/// Release an interface-stats snapshot. `lev_interface_stats_free(NULL)` is a
/// no-op.
#[no_mangle]
pub unsafe extern "C" fn lev_interface_stats_free(table: *mut lev_interface_stats_t) {
    guard((), || {
        if !table.is_null() {
            drop(Box::from_raw(table));
        }
    })
}
