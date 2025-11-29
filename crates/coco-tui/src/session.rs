use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use snafu::prelude::*;

use crate::error::Result;

mod registries;
pub use registries::*;

/// A serialized session state that can be saved and restored.
///
/// This type alias represents session data that has been serialized to JSON format,
/// allowing components to persist their state across application sessions.
pub type Session = Value;

/// Serializes a value into a session-compatible JSON value.
///
/// # Arguments
/// * `session` - The value to serialize
///
/// # Panics
/// Panics if serialization fails
pub fn save<T>(session: T) -> Session
where
    T: Serialize,
{
    serde_json::to_value(session).expect("failed to save session")
}

/// Deserializes a session JSON value back into its original type.
///
/// # Arguments
/// * `session` - The session value to deserialize
///
/// # Returns
/// Returns the deserialized value or an error if deserialization fails
pub fn load<T>(session: Session) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_value(session).whatever_context("failed to load session")
}

#[derive(Serialize, Deserialize)]
struct Relate<T, Y> {
    pub t: T,
    pub y: Y,
}

impl<T, Y> Relate<T, Y> {
    pub fn new(t: T, y: Y) -> Self {
        Self { t, y }
    }
}

/// Serializes two related values into a session-compatible JSON value.
///
/// This function creates a relationship between two values of potentially different types
/// and serializes them together into a single session value.
///
/// # Arguments
/// * `t` - The first value to serialize
/// * `y` - The second value to serialize
///
/// # Panics
/// Panics if serialization fails
pub fn save_related<T, Y>(t: T, y: Y) -> Session
where
    T: Serialize,
    Y: Serialize,
{
    let inst = Relate::new(t, y);
    serde_json::to_value(inst).expect("faild to save session")
}

/// Deserializes a session JSON value containing two related values back into their original types.
///
/// This function reverses the `relate` operation, extracting the two related values
/// that were previously stored together in a session.
///
/// # Arguments
/// * `value` - The session value containing the related values
///
/// # Returns
/// Returns a tuple of the two deserialized values or an error if deserialization fails
pub fn load_related<T, Y>(value: Session) -> Result<(T, Y)>
where
    T: DeserializeOwned,
    Y: DeserializeOwned,
{
    let inst: Relate<T, Y> =
        serde_json::from_value(value).whatever_context("failed to load session")?;
    Ok((inst.t, inst.y))
}
