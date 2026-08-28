//! Typed identifiers.
//!
//! Every entity gets its own newtype over [`Uuid`] so that a `WebsiteId` can never be
//! passed where a `ServerId` is expected. The macro keeps them consistent and free of
//! boilerplate.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a fresh random identifier.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Wraps an existing UUID, e.g. one loaded from storage.
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// The underlying UUID.
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// Parses the hyphenated text form.
            pub fn parse(raw: &str) -> Result<Self, IdParseError> {
                Uuid::parse_str(raw)
                    .map(Self)
                    .map_err(|_| IdParseError { kind: stringify!($name), value: raw.to_owned() })
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }
    };
}

/// Raised when a stored identifier is not a valid UUID.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid {kind}: {value:?} is not a UUID")]
pub struct IdParseError {
    pub kind: &'static str,
    pub value: String,
}

define_id!(
    /// Identifies a monitored server.
    ServerId
);
define_id!(
    /// Identifies a monitored website or endpoint.
    WebsiteId
);
define_id!(
    /// Identifies the binding between a website and one analytics provider.
    IntegrationId
);
define_id!(
    /// Identifies an alert rule.
    AlertRuleId
);
define_id!(
    /// Identifies an incident raised by an alert rule.
    IncidentId
);
define_id!(
    /// Identifies a recorded event in the event log.
    EventId
);
define_id!(
    /// Opaque handle to a secret held by the [`crate::ports::SecretStore`].
    ///
    /// The secret itself is never part of the domain model; only this reference is.
    CredentialRef
);

/// Identifies a provider implementation (`"yandex_metrica"`, `"chromium_cli"`, …).
///
/// A string rather than an enum, precisely so that adding a provider does not require
/// editing the domain.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ProviderId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// Identifies a collector implementation (`"cpu"`, `"docker"`, …).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CollectorId(String);

impl CollectorId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CollectorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for CollectorId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        assert_ne!(ServerId::new(), ServerId::new());
    }

    #[test]
    fn ids_round_trip_through_text() {
        let id = WebsiteId::new();
        let parsed = WebsiteId::parse(&id.to_string()).expect("freshly formatted id must parse");
        assert_eq!(id, parsed);
    }

    #[test]
    fn parsing_rejects_non_uuid_text() {
        let err = ServerId::parse("not-a-uuid").expect_err("must reject");
        assert_eq!(err.kind, "ServerId");
    }

    #[test]
    fn ids_serialise_as_bare_strings() {
        let id = ServerId::from_uuid(Uuid::nil());
        let json = serde_json::to_string(&id).expect("uuid always serialises");
        assert_eq!(json, "\"00000000-0000-0000-0000-000000000000\"");
    }
}
