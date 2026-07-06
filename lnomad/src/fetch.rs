//! NomadNet page fetch over shared-instance IPC.
//!
//! A [`Session`] connects to a running `lnsd` or `rnsd` shared instance the same
//! way `lncp`/`lnstatus` do (`ReticulumNodeBuilder::connect_to_shared_instance`),
//! then fetches pages over the raw RNS request/response path. NomadNet serves
//! pages, not over LXMF, but by a destination request handler registered on
//! `/page/<name>.mu`: the client issues a request on an established [`Link`], a
//! response that fits one packet returns as a single `RESPONSE` packet, and a
//! larger response is auto-upgraded by RNS to a Resource carrying `is_response`
//! and the `request_id`. Either way the node surfaces a
//! [`NodeEvent::ResponseReceived`] whose `response_data` is the raw msgpack
//! response value (the inner value only; the `[request_id, response]` wrapper is
//! already stripped by the node).
//!
//! [`Link`]: leviculum_std::LinkHandle
//! [`NodeEvent::ResponseReceived`]: leviculum_std::NodeEvent::ResponseReceived

use std::path::{Path, PathBuf};
use std::time::Duration;

use leviculum_std::config::Config;
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::{DestinationHash, EventReceiver, NodeEvent, ReticulumNode};

use crate::url::Target;

/// Path-discovery budget. A `PATH_REQUEST` has no retry of its own, so this is
/// generous enough to cover a slow shared-instance announce forward while still
/// being bounded.
const PATH_BUDGET: Duration = Duration::from_secs(30);

/// How often [`ReticulumNode::wait_for_path`] re-issues a `PATH_REQUEST` while
/// waiting.
const PATH_RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// Extra slack added on top of the caller's request timeout when waiting for a
/// response event, so the node's own `RequestTimedOut` fires first and surfaces
/// as [`FetchError::Timeout`] rather than a bare deadline.
const RESPONSE_SLACK: Duration = Duration::from_secs(2);

/// Errors from [`Session`] connection and fetching.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The shared instance could not be reached or the node failed to start.
    #[error("could not connect to shared instance: {0}")]
    Connect(String),
    /// No path to the destination could be found within the budget.
    #[error("no path to destination")]
    NoPath,
    /// The link to the destination could not be established or was lost.
    #[error("link failed")]
    LinkFailed,
    /// The request timed out without a response (also the clean outcome for an
    /// unregistered path, where the responder stays silent).
    #[error("request timed out")]
    Timeout,
    /// The responder indicated the path is not served.
    #[error("path not found on destination")]
    NotFound,
    /// The target names a `/file/` download, which is not implemented yet.
    #[error("file downloads are not supported yet")]
    UnsupportedFile,
    /// The response was not a well-formed msgpack page value.
    #[error("malformed response payload")]
    Decode,
    /// The underlying node reported an error dispatching a request or link.
    #[error("node error: {0}")]
    Node(String),
}

/// The current reused link: which destination it reaches and its id.
struct CurrentLink {
    dest_hash: [u8; 16],
    link_id: leviculum_std::LinkId,
}

/// A connected fetch session over a shared instance.
///
/// Owns the connected node and its event stream, and reuses a single [`Link`]
/// across fetches to the same destination the way the reference browser does.
///
/// [`Link`]: leviculum_std::LinkHandle
pub struct Session {
    node: ReticulumNode,
    events: EventReceiver,
    current: Option<CurrentLink>,
}

impl Session {
    /// Connect to the shared instance named in `config_dir/config`, falling back
    /// to instance `default`. Storage is read from `config_dir/storage`, matching
    /// `lncp`; a client with transport disabled writes nothing there.
    pub async fn connect(config_dir: &Path) -> Result<Self, FetchError> {
        let instance_name = read_instance_name(config_dir);
        let storage_path = config_dir.join("storage");
        Self::connect_to(&instance_name, storage_path).await
    }

    /// Connect to an explicitly named shared instance with an explicit storage
    /// path. Used directly by tests that stand up a private instance.
    pub async fn connect_to(
        instance_name: &str,
        storage_path: PathBuf,
    ) -> Result<Self, FetchError> {
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(false)
            .connect_to_shared_instance(instance_name)
            .storage_path(storage_path)
            .build_sync()
            .map_err(|e| FetchError::Connect(e.to_string()))?;
        let events = node
            .take_event_receiver()
            .ok_or_else(|| FetchError::Connect("no event receiver".to_string()))?;
        node.start()
            .await
            .map_err(|e| FetchError::Connect(e.to_string()))?;
        Ok(Self {
            node,
            events,
            current: None,
        })
    }

    /// Fetch a page and return its raw bytes (the `.mu` markup the handler
    /// served), decoded from the single msgpack value RNS packs a `bytes`
    /// response into.
    pub async fn fetch(
        &mut self,
        target: &Target,
        timeout: Duration,
    ) -> Result<Vec<u8>, FetchError> {
        let raw = self.request(target, timeout).await?;
        decode_page(&raw)
    }

    /// Issue the request and return the raw msgpack response value unchanged.
    ///
    /// This is the general request/response primitive: [`fetch`](Self::fetch) is
    /// this plus page decoding. It is exposed for callers that need the raw
    /// response value, e.g. a handler that echoes back its request fields as a
    /// msgpack map rather than a page `bytes` value.
    pub async fn request(
        &mut self,
        target: &Target,
        timeout: Duration,
    ) -> Result<Vec<u8>, FetchError> {
        if target.is_file {
            return Err(FetchError::UnsupportedFile);
        }

        let link_id = self.ensure_link(&target.dest_hash, timeout).await?;
        let data = encode_fields(&target.fields)?;
        let timeout_ms = timeout.as_millis() as u64;

        let request_id = self
            .node
            .send_request(&link_id, &target.path, data.as_deref(), Some(timeout_ms))
            .await
            .map_err(|e| match e {
                leviculum_std::Error::Request(
                    leviculum_core::RequestError::LinkNotActive
                    | leviculum_core::RequestError::LinkNotFound,
                ) => FetchError::LinkFailed,
                other => FetchError::Node(other.to_string()),
            })?;

        self.await_response(request_id, link_id, timeout + RESPONSE_SLACK)
            .await
    }

    /// Establish (or reuse) an active link to `dest`, learning a path and the
    /// destination identity first if needed.
    async fn ensure_link(
        &mut self,
        dest: &[u8; 16],
        timeout: Duration,
    ) -> Result<leviculum_std::LinkId, FetchError> {
        if let Some(current) = &self.current {
            if current.dest_hash == *dest && self.node.link_mdu(&current.link_id).is_some() {
                return Ok(current.link_id);
            }
        }
        self.current = None;

        let dest_hash = DestinationHash::new(*dest);

        // Learn a path if we do not have one.
        if !self.node.has_path(&dest_hash) {
            self.node
                .request_path(&dest_hash)
                .await
                .map_err(|e| FetchError::Node(e.to_string()))?;
            let found = self
                .node
                .wait_for_path(&dest_hash, PATH_BUDGET, PATH_RETRY_INTERVAL)
                .await
                .map_err(|e| FetchError::Node(e.to_string()))?;
            if !found {
                return Err(FetchError::NoPath);
            }
        }

        // The destination identity (learned from its announce) yields the
        // Ed25519 verifying key needed to verify the link proof.
        let identity = self
            .node
            .get_identity(&dest_hash)
            .ok_or(FetchError::NoPath)?;
        let pk = identity.public_key_bytes();
        let mut signing_key = [0u8; 32];
        signing_key.copy_from_slice(&pk[32..64]);

        let handle = self
            .node
            .connect(&dest_hash, &signing_key)
            .await
            .map_err(|_| FetchError::LinkFailed)?;
        let link_id = *handle.link_id();

        self.wait_for_link_established(link_id, timeout).await?;

        self.current = Some(CurrentLink {
            dest_hash: *dest,
            link_id,
        });
        Ok(link_id)
    }

    /// Wait for the initiator-side `LinkEstablished` for `link_id`, or a clean
    /// [`FetchError::LinkFailed`] on close or deadline.
    async fn wait_for_link_established(
        &mut self,
        link_id: leviculum_std::LinkId,
        timeout: Duration,
    ) -> Result<(), FetchError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let event = match tokio::time::timeout_at(deadline, self.events.recv()).await {
                Ok(Some(event)) => event,
                Ok(None) => return Err(FetchError::LinkFailed),
                Err(_) => return Err(FetchError::LinkFailed),
            };
            match event {
                NodeEvent::LinkEstablished {
                    link_id: id,
                    is_initiator,
                } if id == link_id && is_initiator => return Ok(()),
                NodeEvent::LinkClosed { link_id: id, .. } if id == link_id => {
                    return Err(FetchError::LinkFailed)
                }
                _ => {}
            }
        }
    }

    /// Wait for the `ResponseReceived` matching `request_id`, mapping a
    /// `RequestTimedOut` to [`FetchError::Timeout`] and a link loss to
    /// [`FetchError::LinkFailed`].
    async fn await_response(
        &mut self,
        request_id: [u8; 16],
        link_id: leviculum_std::LinkId,
        timeout: Duration,
    ) -> Result<Vec<u8>, FetchError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let event = match tokio::time::timeout_at(deadline, self.events.recv()).await {
                Ok(Some(event)) => event,
                Ok(None) => return Err(FetchError::LinkFailed),
                Err(_) => return Err(FetchError::Timeout),
            };
            match event {
                NodeEvent::ResponseReceived {
                    request_id: id,
                    response_data,
                    ..
                } if id == request_id => return Ok(response_data),
                NodeEvent::RequestTimedOut { request_id: id, .. } if id == request_id => {
                    return Err(FetchError::Timeout)
                }
                NodeEvent::LinkClosed { link_id: id, .. } if id == link_id => {
                    self.current = None;
                    return Err(FetchError::LinkFailed);
                }
                _ => {}
            }
        }
    }

    /// Stop the node and tear down the connection.
    pub async fn close(mut self) -> Result<(), FetchError> {
        self.node
            .stop()
            .await
            .map_err(|e| FetchError::Node(e.to_string()))
    }
}

/// Read the shared-instance name from `config_dir/config`, defaulting to
/// `default` when the file is missing or unreadable (matching `lncp`).
fn read_instance_name(config_dir: &Path) -> String {
    let config_file = config_dir.join("config");
    if config_file.exists() {
        if let Ok(config) = Config::load(&config_file) {
            return config.reticulum.instance_name;
        }
    }
    "default".to_string()
}

/// Encode query fields as a single msgpack map value (`{var_key: value}`),
/// matching how NomadNet passes URL query variables as the request `data`.
/// Returns `None` when there are no fields, so the request carries a nil payload.
fn encode_fields(fields: &[(String, String)]) -> Result<Option<Vec<u8>>, FetchError> {
    if fields.is_empty() {
        return Ok(None);
    }
    let map: Vec<(rmpv::Value, rmpv::Value)> = fields
        .iter()
        .map(|(k, v)| {
            (
                rmpv::Value::String(k.as_str().into()),
                rmpv::Value::String(v.as_str().into()),
            )
        })
        .collect();
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &rmpv::Value::Map(map)).map_err(|_| FetchError::Decode)?;
    Ok(Some(buf))
}

/// Decode a page response: RNS packs a `bytes` page as a single msgpack bin, so
/// the raw response value is exactly that bin.
fn decode_page(response_data: &[u8]) -> Result<Vec<u8>, FetchError> {
    let mut cursor = std::io::Cursor::new(response_data);
    match rmpv::decode::read_value(&mut cursor).map_err(|_| FetchError::Decode)? {
        rmpv::Value::Binary(bytes) => Ok(bytes),
        _ => Err(FetchError::Decode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_fields_empty_is_none() {
        assert!(encode_fields(&[]).unwrap().is_none());
    }

    #[test]
    fn encode_then_decode_page_roundtrips_bytes() {
        // A page bin encodes and decodes back to the same bytes.
        let page = b"# hello\n".to_vec();
        let mut buf = Vec::new();
        rmpv::encode::write_value(&mut buf, &rmpv::Value::Binary(page.clone())).unwrap();
        assert_eq!(decode_page(&buf).unwrap(), page);
    }

    #[test]
    fn decode_page_rejects_non_binary() {
        let mut buf = Vec::new();
        rmpv::encode::write_value(&mut buf, &rmpv::Value::from(42)).unwrap();
        assert!(matches!(decode_page(&buf), Err(FetchError::Decode)));
    }

    #[test]
    fn encode_fields_builds_a_map() {
        let fields = vec![("var_a".to_string(), "1".to_string())];
        let encoded = encode_fields(&fields).unwrap().unwrap();
        let mut cursor = std::io::Cursor::new(encoded.as_slice());
        let value = rmpv::decode::read_value(&mut cursor).unwrap();
        match value {
            rmpv::Value::Map(entries) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].0.as_str(), Some("var_a"));
                assert_eq!(entries[0].1.as_str(), Some("1"));
            }
            other => panic!("expected map, got {other:?}"),
        }
    }
}
