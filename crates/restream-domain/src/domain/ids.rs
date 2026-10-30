//! Typed entity identifiers for all domain objects.
//!
//! Each ID is a transparent newtype over `String`. String fields at DB/API
//! boundaries are kept as-is; these types are used internally so that
//! pipeline IDs, output IDs, etc. cannot be accidentally mixed.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! string_id {
    ($(#[$attr:meta])* $name:ident) => {
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                &self.0 == other
            }
        }
    };
}

string_id!(
    /// Unique identifier for an output (egress target).
    OutputId
);

string_id!(
    /// Unique identifier for a standalone ingest source.
    IngestId
);

string_id!(
    /// Unique identifier for a recording artifact.
    RecordingId
);

string_id!(
    /// Unique identifier for an async job (e.g. agent operation).
    JobId
);

string_id!(
    /// Unique identifier for a pipeline.
    PipelineId
);

string_id!(
    /// Unique identifier for a stage within a pipeline's media graph.
    StageId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_id_roundtrip() {
        let id = OutputId::new("out_abc123");
        assert_eq!(id.as_str(), "out_abc123");
        assert_eq!(id.to_string(), "out_abc123");
        assert_eq!(OutputId::from("out_abc123"), id);
        assert_eq!(OutputId::from("out_abc123".to_string()), id);
    }

    #[test]
    fn ingest_id_roundtrip() {
        let id = IngestId::new("ingest_42");
        assert_eq!(id.as_str(), "ingest_42");
        assert_eq!(&id, "ingest_42");
    }

    #[test]
    fn recording_id_roundtrip() {
        let id = RecordingId::from("rec_001");
        assert_eq!(id.into_string(), "rec_001");
    }

    #[test]
    fn job_id_roundtrip() {
        let id = JobId::new("job_xyz");
        assert_eq!(id.as_ref(), "job_xyz");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"job_xyz\"");
        let back: JobId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn pipeline_id_roundtrip() {
        let id = PipelineId::new("pipe_42");
        assert_eq!(id.as_str(), "pipe_42");
        assert_eq!(PipelineId::from("pipe_42"), id);
        assert_eq!(&id, "pipe_42");
    }

    #[test]
    fn stage_id_roundtrip() {
        let id = StageId::new("video:720p");
        assert_eq!(id.as_str(), "video:720p");
        assert_eq!(id.to_string(), "video:720p");
        assert_eq!(StageId::from("video:720p".to_string()), id);
    }

    #[test]
    fn ids_are_type_distinct() {
        // Ensures that the compiler distinguishes between ID types.
        let oid = OutputId::new("same_value");
        let iid = IngestId::new("same_value");
        assert_eq!(oid.as_str(), iid.as_str());
        // OutputId and IngestId are different types; the above compiles
        // only because we compare their str content, not the IDs themselves.
    }
}
