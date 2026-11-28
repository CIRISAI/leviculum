//! What a direct-link probe found and what a punch cost (+ciris), for the
//! host: carried by `NodeEvent::DirectLinkProbed`,
//! `NodeEvent::DirectLinkEstablished` and `NodeEvent::DirectLinkFailed`.
//! Classes and counts only: no address and no port is ever in them.

/// This NAT's mapping, from the session socket's own mappings toward two
/// observers. A NAT that gives each socket a random port but keeps one
/// mapping per socket (an endpoint-independent CGNAT) differs only between
/// sockets, so only the same socket toward two destinations can tell it
/// from one that maps per destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NatClass {
    /// The first observer saw the socket's own address.
    NoNat,
    /// Both observers saw the same mapping.
    EndpointIndependent,
    /// The two observers saw different mappings.
    EndpointDependent,
    /// Not decidable: no second observer answered (or none is
    /// configured), or the facilitator itself did not answer.
    Unknown,
}

impl NatClass {
    /// Stable lowercase name, as the diagnostic line logs it.
    pub const fn as_str(self) -> &'static str {
        match self {
            NatClass::NoNat => "no_nat",
            NatClass::EndpointIndependent => "endpoint_independent",
            NatClass::EndpointDependent => "endpoint_dependent",
            NatClass::Unknown => "unknown",
        }
    }
}

/// On what evidence this node holds its NAT to be a random
/// endpoint-dependent one (Eric's ruling on review item 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdmEvidence {
    /// The session socket's mapping moved between two observers. (Two ports
    /// on one IP detect a port-dependent mapping, not an address-only one.)
    TwoObserver,
    /// No observer could tell; our own allocation probes look random. The
    /// birthday phase then opens only once the punch has been silent past
    /// ESCALATE_AFTER, the rest of the evidence.
    FallbackSilenceRandom,
    /// Nothing says the NAT is random.
    None,
}

impl EdmEvidence {
    /// The word logged.
    pub const fn as_str(self) -> &'static str {
        match self {
            EdmEvidence::TwoObserver => "two_observer",
            EdmEvidence::FallbackSilenceRandom => "fallback_silence_random",
            EdmEvidence::None => "none",
        }
    }
}

/// What this side's punch sent beyond its plain punches: all zero when no
/// punch ran on this side (an upgrade that failed before punching).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PunchCounts {
    /// Knocks sent: to the peer's predicted ports and the spray's random
    /// ports.
    pub knocks: u32,
    /// Random ports sprayed on the peer's IP (each one knock).
    pub sprayed_ports: u32,
    /// Birthday sockets this side opened toward the peer (its own NAT is the
    /// random one).
    pub birthday_sockets: u32,
}

/// What a probe found, as the driver hands it to
/// `NodeCore::direct_link_probed_found`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeFindings {
    /// The NAT's mapping.
    pub mapping: NatClass,
    /// On what evidence the NAT is held random.
    pub evidence: EdmEvidence,
    /// A second observer answered the probe.
    pub second_observer_answered: bool,
}
