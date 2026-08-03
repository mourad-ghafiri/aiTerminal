//! Era negotiation — the dual-era client the 2026-07-28 spec describes, as pure
//! decisions.
//!
//! The probe is one `server/discover` request. What comes back sorts every server in
//! the world into one of three outcomes, and the sorting rule is exactly the spec's
//! (*stdio § Backward Compatibility*):
//!
//! - a `DiscoverResult` → a **modern** server; pick a version from its list;
//! - a recognised modern error (`UnsupportedProtocolVersionError`) → still modern;
//!   retry from its `supported` list, and **never** fall back to `initialize`;
//! - anything else — an implementation-defined error, or silence — → a **legacy**
//!   server; fall back to the `initialize` handshake. The spec is explicit that this
//!   arm must NOT key on one specific error code, because legacy servers answer
//!   unknown pre-`initialize` methods with whatever they like, or not at all.

use super::wire::{Era, RpcError, LEGACY_KNOWN, MODERN, UNSUPPORTED_VERSION};
use corelib::wire::Json;

/// What the probe decided.
#[derive(Debug, PartialEq)]
pub(crate) enum Probe {
    /// The server is modern; speak this version.
    Modern(String),
    /// The server answered `initialize`-era rules; run the legacy handshake.
    Legacy,
    /// No mutually supported version exists — a real error, named for the user.
    Incompatible(String),
}

/// Judge a `server/discover` reply.
pub(crate) fn judge_discover(reply: Result<Json, RpcError>) -> Probe {
    match reply {
        Ok(result) => {
            let offered: Vec<String> = result
                .get("supportedVersions")
                .and_then(Json::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            pick(&offered, true)
        }
        Err(e) if e.code == UNSUPPORTED_VERSION => pick(&e.supported, false),
        // Implementation-defined error or timeout-shaped failure: legacy.
        Err(_) => Probe::Legacy,
    }
}

/// Choose from a server's advertised versions. `may_fall_back` is true only for a
/// `DiscoverResult`: a server that ANSWERS discover but offers only legacy revisions
/// is dual-era, and `initialize` is how it serves them. After a modern *error*, the
/// spec forbids falling back — the server is modern, full stop.
fn pick(offered: &[String], may_fall_back: bool) -> Probe {
    if offered.iter().any(|v| v == MODERN) {
        return Probe::Modern(MODERN.to_string());
    }
    if may_fall_back && offered.iter().any(|v| LEGACY_KNOWN.contains(&v.as_str())) {
        return Probe::Legacy;
    }
    Probe::Incompatible(match offered.is_empty() {
        true => "the server advertised no supported protocol version".to_string(),
        false => format!("no mutually supported protocol version — the server offers {}", offered.join(", ")),
    })
}

/// Judge an `initialize` reply (the legacy handshake's first half).
///
/// The server answers with the one version it chose. Any revision in
/// [`LEGACY_KNOWN`] is accepted — the tools flow is identical across all of them —
/// and anything else is refused by name, because "connected but silently wrong"
/// is the failure mode this whole module exists to remove.
pub(crate) fn judge_initialize(result: &Json) -> Result<Era, String> {
    let version = result.get("protocolVersion").and_then(Json::as_str).unwrap_or_default();
    match LEGACY_KNOWN.contains(&version) {
        true => Ok(Era::Legacy(version.to_string())),
        false => Err(match version.is_empty() {
            true => "the server's initialize reply named no protocol version".to_string(),
            false => format!("the server negotiated protocol version {version:?}, which this client does not speak"),
        }),
    }
}
