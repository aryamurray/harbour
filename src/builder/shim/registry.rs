//! Backend registry - lazy discovery and management of build backends.
//!
//! Key principle: Registry construction never fails. Backend availability
//! is checked lazily when the backend is actually needed.

use std::collections::HashMap;

use anyhow::Result;

use crate::builder::shim::capabilities::BackendId;
use crate::builder::shim::cmake_shim::CMakeShim;
use crate::builder::shim::custom_shim::CustomShim;
use crate::builder::shim::native_shim::NativeShim;
use crate::builder::shim::trait_def::{BackendAvailability, BackendShim};
use crate::core::target::BuildRecipe;

/// Registry of available build backends.
///
/// The registry always constructs successfully - it registers all built-in
/// backends without checking if they're actually available. Availability
/// is checked lazily via `shim.availability()` when the backend is needed.
pub struct BackendRegistry {
    backends: HashMap<BackendId, Box<dyn BackendShim>>,
}

impl BackendRegistry {
    /// Create a new registry with all built-in backends.
    ///
    /// This always succeeds - no I/O or detection happens here.
    pub fn new() -> Self {
        let mut registry = BackendRegistry {
            backends: HashMap::new(),
        };

        // Register all built-in backends
        registry.register(Box::new(NativeShim::new()));
        registry.register(Box::new(CMakeShim::new()));
        registry.register(Box::new(CustomShim::new()));

        registry
    }

    /// Register a backend shim.
    pub fn register(&mut self, shim: Box<dyn BackendShim>) {
        let id = shim.capabilities().identity.id;
        self.backends.insert(id, shim);
    }

    /// Get a backend by ID.
    pub fn get(&self, id: BackendId) -> Option<&dyn BackendShim> {
        self.backends.get(&id).map(|b| b.as_ref())
    }

    /// Get a mutable reference to a backend by ID.
    pub fn get_mut(&mut self, id: BackendId) -> Option<&mut Box<dyn BackendShim>> {
        self.backends.get_mut(&id)
    }

    /// Select the appropriate backend for a build recipe.
    pub fn select_for_recipe(&self, recipe: &BuildRecipe) -> Option<&dyn BackendShim> {
        let id = match recipe {
            BuildRecipe::Native => BackendId::Native,
            BuildRecipe::CMake { .. } => BackendId::CMake,
            BuildRecipe::Custom { .. } => BackendId::Custom,
        };
        self.get(id)
    }

    /// Get all registered backend IDs.
    pub fn ids(&self) -> impl Iterator<Item = BackendId> + '_ {
        self.backends.keys().copied()
    }

    /// Get all registered backends.
    pub fn all(&self) -> impl Iterator<Item = &dyn BackendShim> + '_ {
        self.backends.values().map(|b| b.as_ref())
    }

    /// Check availability of all backends.
    ///
    /// This actually runs detection (e.g., `cmake --version`) and may be slow.
    /// Use this for `harbour backend list` or similar commands.
    pub fn check_all(&self) -> Vec<(BackendId, Result<BackendAvailability>)> {
        self.backends
            .iter()
            .map(|(id, shim)| (*id, shim.availability()))
            .collect()
    }

    /// Get the number of registered backends.
    pub fn len(&self) -> usize {
        self.backends.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }

    /// Check if a backend is registered.
    pub fn contains(&self, id: BackendId) -> bool {
        self.backends.contains_key(&id)
    }
}

impl Default for BackendRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of a backend for display purposes.
#[derive(Debug, Clone)]
pub struct BackendSummary {
    /// Backend identifier
    pub id: BackendId,

    /// Availability status
    pub availability: BackendAvailability,

    /// Brief description
    pub description: String,

    /// Whether cross-compilation is supported
    pub cross_compile: bool,

    /// Whether configure phase is required
    pub requires_configure: bool,
}

impl BackendSummary {
    /// Create a summary from a backend shim.
    pub fn from_shim(shim: &dyn BackendShim) -> Result<Self> {
        let caps = shim.capabilities();
        let availability = shim.availability()?;

        let description = match caps.identity.id {
            BackendId::Native => "Harbour's built-in C/C++ compiler driver",
            BackendId::CMake => "CMake-based builds",
            BackendId::Meson => "Meson-based builds",
            BackendId::Custom => "User-defined build commands",
        }
        .to_string();

        Ok(BackendSummary {
            id: caps.identity.id,
            availability,
            description,
            cross_compile: caps.platform.cross_compile,
            requires_configure: caps.phases.configure.is_required(),
        })
    }
}

/// Get all backend summaries.
pub fn get_backend_summaries(registry: &BackendRegistry) -> Vec<BackendSummary> {
    registry
        .all()
        .filter_map(|shim| BackendSummary::from_shim(shim).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_creation() {
        let registry = BackendRegistry::new();
        assert!(!registry.is_empty());
        assert!(registry.contains(BackendId::Native));
        assert!(registry.contains(BackendId::CMake));
        assert!(registry.contains(BackendId::Custom));
    }

    #[test]
    fn test_registry_get() {
        let registry = BackendRegistry::new();

        let native = registry.get(BackendId::Native);
        assert!(native.is_some());
        assert_eq!(
            native.unwrap().capabilities().identity.id,
            BackendId::Native
        );

        let cmake = registry.get(BackendId::CMake);
        assert!(cmake.is_some());
        assert_eq!(cmake.unwrap().capabilities().identity.id, BackendId::CMake);
    }

    #[test]
    fn test_select_for_recipe() {
        let registry = BackendRegistry::new();

        let native = registry.select_for_recipe(&BuildRecipe::Native);
        assert!(native.is_some());
        assert_eq!(
            native.unwrap().capabilities().identity.id,
            BackendId::Native
        );

        let cmake = registry.select_for_recipe(&BuildRecipe::CMake {
            source_dir: None,
            args: Vec::new(),
            targets: Vec::new(),
        });
        assert!(cmake.is_some());
        assert_eq!(cmake.unwrap().capabilities().identity.id, BackendId::CMake);
    }

    #[test]
    fn test_registry_ids() {
        let registry = BackendRegistry::new();
        let ids: Vec<_> = registry.ids().collect();

        assert!(ids.contains(&BackendId::Native));
        assert!(ids.contains(&BackendId::CMake));
        assert!(ids.contains(&BackendId::Custom));
    }

    #[test]
    fn test_backend_summary() {
        let registry = BackendRegistry::new();
        let native = registry.get(BackendId::Native).unwrap();

        let summary = BackendSummary::from_shim(native).unwrap();
        assert_eq!(summary.id, BackendId::Native);
        assert!(!summary.cross_compile);
        assert!(!summary.requires_configure);
    }

    #[test]
    fn test_custom_registration() {
        let mut registry = BackendRegistry::new();
        let initial_count = registry.len();

        // Re-registering with same ID replaces
        registry.register(Box::new(NativeShim::new()));
        assert_eq!(registry.len(), initial_count);
    }
}
