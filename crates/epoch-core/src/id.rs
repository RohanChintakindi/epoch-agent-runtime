use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! domain_id {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

domain_id!(SessionId);
domain_id!(BranchId);
domain_id!(EpochId);
domain_id!(EventId);
domain_id!(CapabilityId);
domain_id!(EffectId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_round_trip_through_strings() {
        let original = SessionId::new();
        let parsed: SessionId = original.to_string().parse().expect("valid UUID");
        assert_eq!(parsed, original);
    }

    #[test]
    fn identifier_types_are_not_interchangeable() {
        let session = SessionId::new();
        let branch: BranchId = session.to_string().parse().expect("valid UUID");
        assert_eq!(branch.as_uuid(), session.as_uuid());
        assert_ne!(
            std::any::TypeId::of::<SessionId>(),
            std::any::TypeId::of::<BranchId>()
        );
    }
}
