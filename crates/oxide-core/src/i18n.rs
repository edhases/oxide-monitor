//! Localization for Oxide Monitor.
//!
//! Uses [Fluent](https://projectfluent.org/) instead of JSON/TOML/YAML: it keeps
//! natural-language grammar (plurals, genders, punctuation) in the translator's
//! hands and has a well-defined fallback chain. Resources are embedded at build
//! time, so there is no runtime file lookup and no locale-file injection risk.

use fluent_bundle::{FluentBundle, FluentResource};
use unic_langid::{LanguageIdentifier, langid};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    En,
    #[default]
    Uk,
    Ru,
}

impl Language {
    pub fn from_code(code: &str) -> Self {
        match code.trim().to_ascii_lowercase().as_str() {
            "uk" | "ua" | "uk-ua" | "uk_ua" => Self::Uk,
            "ru" | "ru-ru" | "ru_ru" => Self::Ru,
            _ => Self::En,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Uk => "uk",
            Self::Ru => "ru",
        }
    }

    fn id(self) -> LanguageIdentifier {
        match self {
            Self::En => langid!("en"),
            Self::Uk => langid!("uk"),
            Self::Ru => langid!("ru"),
        }
    }

    fn source(self) -> &'static str {
        match self {
            Self::En => include_str!("../locales/en.ftl"),
            Self::Uk => include_str!("../locales/uk.ftl"),
            Self::Ru => include_str!("../locales/ru.ftl"),
        }
    }
}

pub struct Localizer {
    language: Language,
    primary: FluentBundle<FluentResource>,
    fallback: FluentBundle<FluentResource>,
}

impl Localizer {
    /// Create a localizer for `code` (`uk`, `en`, `ru`, …). Unknown languages
    /// fall back to English. English is always available as a secondary bundle.
    pub fn new(code: &str) -> Self {
        let language = Language::from_code(code);
        let primary = bundle_for(language);
        let fallback = bundle_for(Language::En);
        Self {
            language,
            primary,
            fallback,
        }
    }

    pub fn language(&self) -> Language {
        self.language
    }

    /// Translate a message id. Missing keys are returned as-is, never as a panic.
    pub fn text(&self, id: &str) -> String {
        format_bundle(&self.primary, id)
            .or_else(|| format_bundle(&self.fallback, id))
            .unwrap_or_else(|| id.to_string())
    }
}

impl Default for Localizer {
    fn default() -> Self {
        Self::new(Language::default().code())
    }
}

fn bundle_for(language: Language) -> FluentBundle<FluentResource> {
    let mut bundle = FluentBundle::new(vec![language.id()]);
    let resource = FluentResource::try_new(language.source().to_string())
        .unwrap_or_else(|_| FluentResource::try_new(String::new()).expect("empty FTL is valid"));
    if let Err(errors) = bundle.add_resource(resource) {
        tracing::error!(
            language = language.code(),
            errors = ?errors,
            "failed to load embedded Fluent resource"
        );
    }
    bundle
}

fn format_bundle(bundle: &FluentBundle<FluentResource>, id: &str) -> Option<String> {
    let message = bundle.get_message(id)?;
    let pattern = message.value()?;
    let mut errors = Vec::new();
    let value = bundle.format_pattern(pattern, None, &mut errors);
    if !errors.is_empty() {
        tracing::debug!(id, ?errors, "Fluent formatting errors");
    }
    Some(value.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_menu() {
        let localizer = Localizer::new("en");
        assert_eq!(localizer.text("menu-exit"), "Exit");
        assert_eq!(localizer.text("menu-show"), "Show widget");
    }

    #[test]
    fn ukrainian_menu() {
        let localizer = Localizer::new("uk");
        assert_eq!(localizer.text("menu-exit"), "Вихід");
        assert_eq!(localizer.text("menu-show"), "Показати віджет");
        assert!(localizer.text("hidden-notice").contains("Віджет приховано"));
    }

    #[test]
    fn unknown_language_falls_back_to_english() {
        let localizer = Localizer::new("de");
        assert_eq!(localizer.language(), Language::En);
        assert_eq!(localizer.text("menu-exit"), "Exit");
    }

    #[test]
    fn missing_key_returns_key() {
        let localizer = Localizer::new("uk");
        assert_eq!(localizer.text("does-not-exist"), "does-not-exist");
    }
}
