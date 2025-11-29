use std::{collections::HashMap, sync::Mutex};

use lazy_static::lazy_static;
use snafu::prelude::*;

use super::Session;
use crate::{
    components::{Component, ContentComponent},
    error::Result,
};

type Registry = HashMap<&'static str, fn(Session) -> Result<Box<dyn Component>>>;
type ContentRegistry = HashMap<&'static str, fn(Session) -> Result<Box<dyn ContentComponent>>>;

lazy_static! {
    static ref COMPONENT_REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());
    static ref CONTENT_COMPONENT_REGISTRY: Mutex<ContentRegistry> =
        Mutex::new(ContentRegistry::new());
}

/// Registers a component factory function for the given type identifier.
///
/// # Arguments
///
/// * `type_id` - The unique identifier for the component type
/// * `load` - The factory function that creates the component
pub fn register_component(type_id: &'static str, load: fn(Session) -> Result<Box<dyn Component>>) {
    let mut registry = COMPONENT_REGISTRY.lock().unwrap();
    registry.insert(type_id, load);
}

/// Registers a content component factory function for the given type identifier.
///
/// # Arguments
///
/// * `type_id` - The unique identifier for the content component type
/// * `load` - The factory function that creates the content component
pub fn register_content_component(
    type_id: &'static str,
    load: fn(Session) -> Result<Box<dyn ContentComponent>>,
) {
    let mut registry = CONTENT_COMPONENT_REGISTRY.lock().unwrap();
    registry.insert(type_id, load);
}

/// Loads a component by its type identifier using the provided session.
///
/// # Arguments
///
/// * `type_id` - The identifier of the component type to load
/// * `session` - The session context for component creation
///
/// # Returns
///
/// Returns a boxed component if the type identifier is registered
///
/// # Errors
///
/// Returns an error if the type identifier is not found in the registry
pub fn load_component(type_id: &str, session: Session) -> Result<Box<dyn Component>> {
    let registry = COMPONENT_REGISTRY.lock().unwrap();
    let load = registry
        .get(type_id)
        .whatever_context(format!("Unknown type_id: {type_id}"))?;

    load(session)
}

/// Loads a content component by its type identifier using the provided session.
///
/// # Arguments
///
/// * `type_id` - The identifier of the content component type to load
/// * `session` - The session context for component creation
///
/// # Returns
///
/// Returns a boxed content component if the type identifier is registered
///
/// # Errors
///
/// Returns an error if the type identifier is not found in the registry
pub fn load_content_component(
    type_id: &str,
    session: Session,
) -> Result<Box<dyn ContentComponent>> {
    let registry = CONTENT_COMPONENT_REGISTRY.lock().unwrap();
    let load = registry
        .get(type_id)
        .whatever_context(format!("Unknown type_id: {type_id}"))?;

    load(session)
}
