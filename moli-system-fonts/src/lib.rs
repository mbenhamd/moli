// SPDX-License-Identifier: MIT OR Apache-2.0

//! Browser-compatible system font family substitutions for Fontique collections.
//!
//! Native matching supplies candidates; the matching policy accepts only exact
//! names, configured renames, and known metric-compatible replacements. An
//! unrelated best-effort match leaves the requested name unchanged so the caller
//! can try the next CSS family. Font list ordering, downloadable fonts, shaping,
//! and glyph fallback remain with the caller.

use std::collections::HashMap;

use fontique::Collection;

#[cfg(any(test, target_os = "linux", target_os = "freebsd"))]
mod matching;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[path = "fontconfig.rs"]
mod platform;

/// Caches named-family substitutions for one font collection.
///
/// Create the resolver before registering downloadable fonts: the initial
/// snapshot limits native candidates to system families. Existing collection
/// families take precedence over platform substitutions. Recreate the resolver
/// when rebuilding the collection; invalidate substitutions when adding fonts
/// after a cached lookup.
pub struct SystemFontFamilyResolver {
    system_families: HashMap<String, String>,
    substitutions: HashMap<String, Option<String>>,
    #[cfg(test)]
    family_lookup_count: usize,
}

impl SystemFontFamilyResolver {
    /// Clears cached matches after font arrival without rescanning system families.
    pub fn invalidate_substitutions(&mut self) {
        self.substitutions.clear();
    }

    /// Snapshots the collection's system family names without performing matching.
    pub fn new(collection: &mut Collection) -> Self {
        let system_families = collection
            .family_names()
            .map(|name| (normalized_family_name(name), name.to_owned()))
            .collect();
        Self {
            system_families,
            substitutions: HashMap::new(),
            #[cfg(test)]
            family_lookup_count: 0,
        }
    }

    /// Returns an accepted replacement for a named CSS family.
    ///
    /// `None` means to preserve the requested name: either the collection already
    /// knows it, or no acceptable replacement exists. This does not establish
    /// whether a family is installed. Generic CSS families bypass this method.
    /// Both replacements and unchanged results are cached case-insensitively.
    pub fn substitute_family(
        &mut self,
        collection: &mut Collection,
        family: &str,
    ) -> Option<String> {
        let key = normalized_family_name(family);
        if let Some(cached) = self.substitutions.get(&key) {
            return cached.clone();
        }
        #[cfg(test)]
        {
            self.family_lookup_count = self.family_lookup_count.saturating_add(1);
        }
        if collection.family_id(family).is_some() {
            self.substitutions.insert(key, None);
            return None;
        }
        self.resolve_missing_family(family)
    }

    /// On Linux, points the `serif` and `sans-serif` generics at the families
    /// Chromium uses by default there ("Times New Roman" and "Arial").
    ///
    /// Each named default goes through the same exact or metric-compatible
    /// substitution as a CSS family, so Liberation Serif and Liberation Sans
    /// stand in when the Microsoft fonts are absent. Without an acceptable
    /// match the generic keeps the platform's own fontconfig choice. The platform
    /// families stay behind the default as per-glyph fallback. Call this before
    /// registering downloadable fonts.
    pub fn apply_browser_generic_defaults(&mut self, collection: &mut Collection) {
        for &(generic, default_family) in BROWSER_GENERIC_DEFAULTS {
            let family = self
                .substitute_family(collection, default_family)
                .unwrap_or_else(|| default_family.to_owned());
            let Some(default_id) = collection.family_id(&family) else {
                continue;
            };
            // Fontique lists collection-owned generic families before the
            // platform's, so the platform list remains the fallback tail.
            collection.set_generic_families(generic, std::iter::once(default_id));
        }
    }

    fn resolve_missing_family(&mut self, family: &str) -> Option<String> {
        let key = normalized_family_name(family);
        if let Some(cached) = self.substitutions.get(&key) {
            return cached.clone();
        }
        let substitution = platform::match_family(family, &self.system_families);
        self.substitutions.insert(key, substitution.clone());
        substitution
    }
}

/// Chromium's Linux default generic font preferences
/// (`IDS_SERIF_FONT_FAMILY` and `IDS_SANS_SERIF_FONT_FAMILY`). Other platforms
/// keep their native generic families.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
const BROWSER_GENERIC_DEFAULTS: &[(fontique::GenericFamily, &str)] = &[
    (fontique::GenericFamily::Serif, "Times New Roman"),
    (fontique::GenericFamily::SansSerif, "Arial"),
];
#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
const BROWSER_GENERIC_DEFAULTS: &[(fontique::GenericFamily, &str)] = &[];

fn normalized_family_name(name: &str) -> String {
    name.chars().flat_map(char::to_lowercase).collect()
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
mod platform {
    pub(super) fn match_family(
        _family: &str,
        _available_families: &std::collections::HashMap<String, String>,
    ) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_arrival_clears_substitutions_but_preserves_the_platform_inventory() {
        let mut resolver = SystemFontFamilyResolver {
            system_families: HashMap::from([("fallback".to_owned(), "Fallback".to_owned())]),
            substitutions: HashMap::from([
                ("arriving".to_owned(), Some("Fallback".to_owned())),
                ("missing".to_owned(), None),
            ]),
            family_lookup_count: 2,
        };
        resolver.invalidate_substitutions();
        assert!(resolver.substitutions.is_empty());
        assert_eq!(resolver.system_families["fallback"], "Fallback");
        assert_eq!(resolver.family_lookup_count, 2);
    }

    #[test]
    fn successful_family_lookup_is_cached() {
        let mut collection = Collection::new(fontique::CollectionOptions {
            shared: false,
            system_fonts: true,
        });
        let family = collection
            .family_names()
            .next()
            .expect("the system font collection should expose at least one family")
            .to_owned();
        let mut resolver = SystemFontFamilyResolver::new(&mut collection);

        for _ in 0..2 {
            assert_eq!(resolver.substitute_family(&mut collection, &family), None);
        }

        assert_eq!(
            resolver.family_lookup_count, 1,
            "a known family must not rebuild Fontique's normalized lookup key"
        );
    }

    #[test]
    fn missing_family_lookup_is_cached_without_replacing_the_css_fallback() {
        let mut collection = Collection::new(fontique::CollectionOptions {
            shared: false,
            system_fonts: true,
        });
        let mut resolver = SystemFontFamilyResolver::new(&mut collection);
        let family = "__moli_missing_font_family_cache_3bb2__";
        for _ in 0..2 {
            assert_eq!(resolver.substitute_family(&mut collection, family), None);
        }
        assert_eq!(resolver.family_lookup_count, 1);
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn fontconfig_aliases_do_not_convert_an_unknown_family_to_default_sans() {
        let mut collection = Collection::new(fontique::CollectionOptions {
            shared: false,
            system_fonts: true,
        });
        let mut resolver = SystemFontFamilyResolver::new(&mut collection);
        assert_eq!(
            resolver.resolve_missing_family("__moli_another_missing_family_92a6__"),
            None
        );

        if resolver
            .system_families
            .contains_key(&normalized_family_name("Liberation Sans"))
        {
            assert_eq!(
                resolver.resolve_missing_family("Arial").as_deref(),
                Some("Liberation Sans")
            );
        }
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn serif_and_sans_serif_generics_start_with_chromium_default_families() {
        let mut collection = Collection::new(fontique::CollectionOptions {
            shared: false,
            system_fonts: true,
        });
        let mut resolver = SystemFontFamilyResolver::new(&mut collection);
        resolver.apply_browser_generic_defaults(&mut collection);

        for (generic, default_family, metric_compatible) in [
            (
                fontique::GenericFamily::Serif,
                "Times New Roman",
                "Liberation Serif",
            ),
            (
                fontique::GenericFamily::SansSerif,
                "Arial",
                "Liberation Sans",
            ),
        ] {
            let expected = [default_family, metric_compatible]
                .into_iter()
                .find_map(|name| collection.family_id(name));
            let Some(expected) = expected else {
                continue;
            };
            assert_eq!(
                collection.generic_families(generic).next(),
                Some(expected),
                "{default_family} or {metric_compatible} must lead {generic:?}"
            );
        }
    }
}
