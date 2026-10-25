//! Modules for use in `#[serde(with = "...")]`.

/// (De)serialize a [`jiff::Timestamp`] as a rounded integer number of seconds.
pub(crate) mod timestamp_rounded_second {
    use jiff::{Timestamp, fmt::serde::timestamp};
    use serde::{Deserializer, Serialize, Serializer};

    use crate::TimestampExt;

    /// Serialize a [`Timestamp`] as a rounded integer number of seconds.
    pub(crate) fn serialize<S>(timestamp: &Timestamp, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        timestamp.as_rounded_second().serialize(serializer)
    }

    /// Deserialize a [`Timestamp`] from an integer number of seconds.
    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Timestamp, D::Error>
    where
        D: Deserializer<'de>,
    {
        timestamp::second::required::deserialize(deserializer)
    }
}
