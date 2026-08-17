//! Where a request would go, and under what model name — validated at
//! construction so no later stage has to wonder.
//!
//! # Reachability is not probed, ever
//!
//! There is no `is_reachable`, no `ping`, and no DNS lookup in this crate, and
//! adding one would invert the gate rather than inform it. ADR-0019 §2:
//!
//! > A reachability probe *is* egress: a DNS lookup leaks the query to a
//! > resolver, and doing it to decide whether egress is permitted inverts the
//! > gate.
//!
//! [`Endpoint::new`] therefore validates the **shape** of a URL and nothing
//! about the world: it never resolves a host, and cannot, because this crate has
//! no transport. Whether the endpoint answers is discovered by the one act that
//! was already consented to — the call itself — and reported as a named failure
//! (`RemoteError::Transport`), never as a quiet fall back to a local model.

use crate::trust::ProducerTrust;

/// A destination for a remote call: the URL, the model string, and how much that
/// model string can be trusted to identify anything ([`ProducerTrust`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    url: String,
    model: String,
    trust: ProducerTrust,
}

/// Why an endpoint could not be accepted.
/// Marked `#[non_exhaustive]` for the reason recorded on
/// [`crate::Reason`]: this crate is published at 1.x, and error sets grow.
/// Taken while the crate had no consumer that could exist; it will not be
/// taken again.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EndpointError {
    /// The URL is empty, or names no scheme this will accept.
    #[error(
        "`{url}` is not a usable remote endpoint: it must be an `https://` URL \
         (or `http://` to a loopback address, for a local gateway) — set `[remote] endpoint`"
    )]
    Scheme {
        /// The URL as configured.
        url: String,
    },
    /// A plaintext URL to something that is not this machine.
    ///
    /// Refused rather than warned about: the payload carries symbol names, paths
    /// and captured prose from a repository, and putting that on the wire in
    /// clear is not a trade-off a warning can make on the operator's behalf.
    #[error(
        "`{url}` would send repository content over plaintext HTTP to `{host}`, which is not \
         this machine — refusing. Use `https://`, or point `[remote] endpoint` at a loopback \
         gateway (`http://127.0.0.1:…`) that terminates TLS itself"
    )]
    PlaintextOffHost {
        /// The URL as configured.
        url: String,
        /// The host it names.
        host: String,
    },
    /// No model string.
    #[error("`[remote] model` is not set: a remote call has to name the model it is asking for")]
    NoModel,
}

/// Hosts that are unambiguously this machine, for the one case plaintext is
/// allowed: a local gateway that terminates TLS on Roteiro's behalf.
const LOOPBACK_HOSTS: [&str; 4] = ["localhost", "127.0.0.1", "[::1]", "::1"];

impl Endpoint {
    /// Build an endpoint, refusing one that cannot carry a payload safely.
    ///
    /// # Errors
    /// [`EndpointError::Scheme`] for a URL that is not `http(s)://…`,
    /// [`EndpointError::PlaintextOffHost`] for plaintext to anywhere but this
    /// machine, and [`EndpointError::NoModel`] for a missing model string.
    pub fn new(url: &str, model: &str, trust: ProducerTrust) -> Result<Self, EndpointError> {
        let url = url.trim();
        let model = model.trim();
        if model.is_empty() {
            return Err(EndpointError::NoModel);
        }
        if let Some(rest) = url.strip_prefix("http://") {
            let host = host_of(rest);
            if !LOOPBACK_HOSTS.contains(&host.as_str()) {
                return Err(EndpointError::PlaintextOffHost {
                    url: url.to_owned(),
                    host,
                });
            }
        } else if !url.strip_prefix("https://").is_some_and(|rest| {
            // A scheme and nothing after it names no host.
            !host_of(rest).is_empty()
        }) {
            return Err(EndpointError::Scheme {
                url: url.to_owned(),
            });
        }
        Ok(Self {
            url: url.to_owned(),
            model: model.to_owned(),
            trust,
        })
    }

    /// The URL a call would go to. Named in every error and in every ledger
    /// entry, because *"what left this machine, and where to?"* is the question
    /// the record exists to answer.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The model string the request asks for.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// How far that model string can be trusted to identify particular weights.
    #[must_use]
    pub fn trust(&self) -> ProducerTrust {
        self.trust
    }
}

/// The host component of a URL remainder (everything after `scheme://`), lowercased
/// and stripped of any port, userinfo and path.
fn host_of(rest: &str) -> String {
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    // A bracketed IPv6 literal keeps its brackets; anything else splits on the
    // first colon, which is the port.
    let host = if authority.starts_with('[') {
        authority
            .split_once(']')
            .map_or(authority, |(h, _)| h)
            .to_owned()
            + "]"
    } else {
        authority
            .split_once(':')
            .map_or(authority, |(h, _)| h)
            .to_owned()
    };
    host.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{Endpoint, EndpointError, host_of};
    use crate::trust::ProducerTrust;

    fn endpoint(url: &str) -> Result<Endpoint, EndpointError> {
        Endpoint::new(url, "a-vendor-model", ProducerTrust::VendorAsserted)
    }

    /// A payload carries symbol names, repo-relative paths and captured prose.
    /// Sending that in clear to another machine is refused outright rather than
    /// warned about — a warning would be making the trade on the operator's
    /// behalf, after the bytes are already on the wire.
    #[test]
    fn plaintext_to_another_machine_is_refused() {
        let err = endpoint("http://models.example.com/v1/chat/completions")
            .expect_err("plaintext off-host");
        let EndpointError::PlaintextOffHost { ref host, .. } = err else {
            panic!("expected PlaintextOffHost, got {err:?}");
        };
        assert_eq!(host, "models.example.com");
        assert!(err.to_string().contains("https://"), "actionable: {err}");
    }

    /// …but a loopback gateway that terminates TLS itself is a real deployment,
    /// and the bytes never leave the machine on that hop.
    #[test]
    fn plaintext_to_loopback_is_allowed() {
        for url in [
            "http://127.0.0.1:8080/v1/chat/completions",
            "http://localhost:1234/v1",
            "http://[::1]:8080/v1",
        ] {
            assert!(endpoint(url).is_ok(), "{url} is this machine");
        }
    }

    /// Anything that is not an absolute `http(s)` URL is refused rather than
    /// guessed at — there is no default scheme, and inventing one would decide
    /// where a repository's content goes.
    #[test]
    fn a_url_without_a_usable_scheme_is_refused() {
        for url in [
            "",
            "models.example.com/v1",
            "ftp://x/y",
            "https://",
            "https:///v1",
        ] {
            assert!(
                matches!(endpoint(url), Err(EndpointError::Scheme { .. })),
                "{url:?} must be refused"
            );
        }
    }

    /// A call has to name the model it is asking for; there is no default,
    /// because a default would be this project choosing a vendor's product.
    #[test]
    fn an_endpoint_without_a_model_is_refused() {
        let err = Endpoint::new("https://x.example/v1", "  ", ProducerTrust::VendorAsserted)
            .expect_err("no model");
        assert_eq!(err, EndpointError::NoModel);
    }

    /// Host extraction has to survive the shapes URLs actually take, because a
    /// mis-parsed host is how a plaintext off-host URL would be let through as
    /// "loopback".
    #[test]
    fn the_host_is_extracted_from_userinfo_ports_and_paths() {
        assert_eq!(host_of("127.0.0.1:8080/v1"), "127.0.0.1");
        assert_eq!(host_of("LocalHost/v1"), "localhost");
        assert_eq!(host_of("user:pw@evil.example/v1"), "evil.example");
        assert_eq!(host_of("[::1]:9/v1"), "[::1]");
        assert_eq!(host_of("host.example?q=1"), "host.example");
        // The case that motivates parsing at all: a userinfo field that *looks*
        // like a loopback host must not be mistaken for one.
        assert!(
            endpoint("http://127.0.0.1@evil.example/v1").is_err(),
            "userinfo is not the host"
        );
    }

    /// The trust travels with the endpoint, because the ledger records it and a
    /// caller must not be able to record a hosted model as digest-pinned by
    /// forgetting to pass it.
    #[test]
    fn the_endpoint_carries_its_producer_trust() {
        let e = endpoint("https://x.example/v1").expect("valid");
        assert_eq!(e.trust(), ProducerTrust::VendorAsserted);
        assert_eq!(e.url(), "https://x.example/v1");
        assert_eq!(e.model(), "a-vendor-model");
    }
}
