//! Looks: a registry of XMP-driven recipes, plus the static [`Identity`] no-op.
//!
//! At pipeline time the registry resolves a [`crate::DevelopParams::look`]
//! string ("identity" or "xmp:<fp>") into a [`ResolvedLook`]. The pipeline
//! handles the strength blend in linear Rec.2020. There are no curated
//! built-in looks anymore — non-trivial looks come from XMP sidecars
//! registered via the TUI.

use std::collections::HashMap;
use std::sync::Arc;

use crate::space::{Buffer, LinearRec2020};
use crate::transform::xmp::XmpRecipe;

/// The reserved built-in id for "no look".
pub const IDENTITY_ID: &str = "identity";

/// Static no-op look. Kept as the default `DevelopParams::look` value and the
/// fallback when an unknown id is resolved.
pub struct Identity;

impl Identity {
    pub fn apply(_image: &mut Buffer<LinearRec2020>) {
        // intentionally empty
    }
}

/// Resolution outcome from [`LookRegistry::resolve`]. The pipeline branches on
/// this to decide what (if anything) to apply.
pub enum ResolvedLook<'a> {
    Identity,
    Xmp(&'a XmpRecipe),
}

/// Runtime registry of XMP-driven looks. Owns each `XmpRecipe` via `Arc` so
/// callers (and `Job`s shipped to the worker) can share the same recipe
/// without cloning.
#[derive(Debug, Default, Clone)]
pub struct LookRegistry {
    xmp: HashMap<String, Arc<XmpRecipe>>,
}

impl LookRegistry {
    pub fn new() -> Self {
        Self {
            xmp: HashMap::new(),
        }
    }

    /// Register an XMP recipe under a slug. Replaces any prior entry with
    /// the same slug.
    pub fn register_xmp(&mut self, slug: String, recipe: XmpRecipe) {
        self.xmp.insert(slug, Arc::new(recipe));
    }

    /// Drop a previously-registered slug. No-op if absent.
    pub fn unregister(&mut self, slug: &str) {
        self.xmp.remove(slug);
    }

    /// Resolve an id string. `"identity"` and unknown ids both map to
    /// `Identity`; an `"xmp:..."` id maps to its registered recipe (or
    /// `Identity` if not registered).
    pub fn resolve(&self, id: &str) -> ResolvedLook<'_> {
        if id == IDENTITY_ID {
            return ResolvedLook::Identity;
        }
        match self.xmp.get(id) {
            Some(recipe) => ResolvedLook::Xmp(recipe.as_ref()),
            None => ResolvedLook::Identity,
        }
    }

    /// True if the id refers to a registered XMP recipe.
    pub fn is_registered(&self, id: &str) -> bool {
        self.xmp.contains_key(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::Buffer;

    #[test]
    fn identity_apply_is_noop() {
        let data: Vec<f32> = (0..24).map(|i| i as f32 * 0.05).collect();
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(data.clone(), 4, 2);
        Identity::apply(&mut buf);
        assert_eq!(buf.data(), data.as_slice());
    }

    #[test]
    fn registry_resolves_identity_for_default_id() {
        let reg = LookRegistry::new();
        assert!(matches!(reg.resolve(IDENTITY_ID), ResolvedLook::Identity));
    }

    #[test]
    fn registry_resolves_xmp_after_register() {
        let mut reg = LookRegistry::new();
        let mut recipe = XmpRecipe::default();
        recipe.name = Some("test".into());
        reg.register_xmp("xmp:1234".into(), recipe);
        match reg.resolve("xmp:1234") {
            ResolvedLook::Xmp(r) => assert_eq!(r.name.as_deref(), Some("test")),
            ResolvedLook::Identity => panic!("expected XMP, got Identity"),
        }
    }

    #[test]
    fn registry_falls_back_to_identity_on_unknown() {
        let reg = LookRegistry::new();
        assert!(matches!(reg.resolve("xmp:nope"), ResolvedLook::Identity));
    }

    #[test]
    fn unregister_removes_entry() {
        let mut reg = LookRegistry::new();
        reg.register_xmp("xmp:1".into(), XmpRecipe::default());
        assert!(reg.is_registered("xmp:1"));
        reg.unregister("xmp:1");
        assert!(!reg.is_registered("xmp:1"));
        assert!(matches!(reg.resolve("xmp:1"), ResolvedLook::Identity));
    }
}
