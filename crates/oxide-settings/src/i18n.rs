//! Embedded Fluent (`uk`/`en`/`ru`) localization for `oxide-settings.exe`.

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};
use unic_langid::langid;

const UK_FTL: &str = include_str!("../locales/uk.ftl");
const EN_FTL: &str = include_str!("../locales/en.ftl");
const RU_FTL: &str = include_str!("../locales/ru.ftl");

pub struct SettingsI18n {
    lang: String,
    primary: FluentBundle<FluentResource>,
    fallback: FluentBundle<FluentResource>,
}

impl SettingsI18n {
    pub fn new(language: &str) -> Self {
        let normalized = normalize_lang(language);
        let fallback = build_bundle(langid!("en"), EN_FTL);
        let primary = match normalized {
            "uk" => build_bundle(langid!("uk"), UK_FTL),
            "ru" => build_bundle(langid!("ru"), RU_FTL),
            _ => build_bundle(langid!("en"), EN_FTL),
        };
        Self {
            lang: normalized.to_string(),
            primary,
            fallback,
        }
    }

    pub fn set_language(&mut self, language: &str) {
        let normalized = normalize_lang(language);
        if self.lang != normalized {
            *self = Self::new(normalized);
        }
    }

    pub fn t(&self, key: &str) -> String {
        if let Some(text) = format_key(&self.primary, key, None) {
            return text;
        }
        if let Some(text) = format_key(&self.fallback, key, None) {
            return text;
        }
        key.to_string()
    }

    pub fn t_args(&self, key: &str, vars: &[(&str, &str)]) -> String {
        let mut args = FluentArgs::new();
        for &(k, v) in vars {
            args.set(k, v);
        }
        if let Some(text) = format_key(&self.primary, key, Some(&args)) {
            return text;
        }
        if let Some(text) = format_key(&self.fallback, key, Some(&args)) {
            return text;
        }
        key.to_string()
    }
}

fn normalize_lang(raw: &str) -> &'static str {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.starts_with("uk") || lower.starts_with("ua") {
        "uk"
    } else if lower.starts_with("ru") {
        "ru"
    } else {
        "en"
    }
}

fn build_bundle(
    lang: unic_langid::LanguageIdentifier,
    source: &str,
) -> FluentBundle<FluentResource> {
    let mut bundle = FluentBundle::new(vec![lang]);
    bundle.set_use_isolating(false);
    if let Ok(res) = FluentResource::try_new(source.to_string()) {
        let _ = bundle.add_resource(res);
    }
    bundle
}

fn format_key(
    bundle: &FluentBundle<FluentResource>,
    key: &str,
    args: Option<&FluentArgs<'_>>,
) -> Option<String> {
    let msg = bundle.get_message(key)?;
    let pattern = msg.value()?;
    let mut errors = Vec::new();
    let val = bundle.format_pattern(pattern, args, &mut errors);
    Some(val.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_uk_en_ru_and_falls_back_to_en() {
        let uk = SettingsI18n::new("uk");
        assert_eq!(uk.t("btn-apply"), "Застосувати");
        assert_eq!(uk.t("btn-cancel"), "Скасувати");

        let en = SettingsI18n::new("en");
        assert_eq!(en.t("btn-apply"), "Apply");

        let ru = SettingsI18n::new("ru");
        assert_eq!(ru.t("btn-apply"), "Применить");

        let unknown = SettingsI18n::new("de-DE");
        assert_eq!(unknown.t("btn-apply"), "Apply");
    }

    #[test]
    fn widget_size_keys_exist_in_uk_en_ru() {
        let keys = [
            "section-widget-size",
            "btn-size-reset",
            "field-size-mode",
            "size-mode-auto",
            "size-mode-fixed-dip",
            "size-mode-exact-px",
            "field-widget-width",
            "field-widget-height",
            "field-height-auto",
            "field-scale-pct",
            "scale-preset-compact",
            "scale-preset-normal",
            "scale-preset-large",
            "scale-preset-tv",
            "field-fit-to-taskbar",
            "tooltip-fit-to-taskbar",
            "size-readout-screen",
        ];
        for lang in ["uk", "en", "ru"] {
            let i18n = SettingsI18n::new(lang);
            for key in keys {
                let text = i18n.t(key);
                assert_ne!(text, key, "missing i18n key `{key}` in locale `{lang}`");
                assert!(
                    !text.trim().is_empty(),
                    "empty i18n key `{key}` in `{lang}`"
                );
            }
        }
    }

    #[test]
    fn autostart_keys_exist_in_uk_en_ru() {
        let plain_keys = [
            "field-autostart",
            "autostart-status-error-unknown",
            "autostart-status-retry-hint",
            "autostart-status-still-registered",
            "autostart-status-disabled",
            "autostart-status-unavailable",
        ];
        for lang in ["uk", "en", "ru"] {
            let i18n = SettingsI18n::new(lang);
            for key in plain_keys {
                let text = i18n.t(key);
                assert_ne!(text, key, "missing i18n key `{key}` in locale `{lang}`");
                assert!(!text.trim().is_empty());
            }
            let enabled = i18n.t_args("autostart-status-enabled", &[("task", "Oxide Monitor")]);
            assert!(enabled.contains("Oxide Monitor"));
            let err = i18n.t_args("autostart-status-error", &[("error", "denied")]);
            assert!(err.contains("denied"));
            let tip_task = i18n.t_args("autostart-tooltip-task", &[("task", "Oxide Monitor")]);
            assert!(tip_task.contains("Oxide Monitor"));
            let tip_upd = i18n.t_args(
                "autostart-tooltip-updated",
                &[("time", "2023-11-14 22:13:20 UTC")],
            );
            assert!(tip_upd.contains("2023-11-14 22:13:20 UTC"));
            let tip_err = i18n.t_args("autostart-tooltip-error", &[("error", "denied")]);
            assert!(tip_err.contains("denied"));
        }
    }

    #[test]
    fn sources_keys_exist_in_uk_en_ru() {
        let plain_keys = [
            "nav-sources",
            "section-sources-config",
            "sources-explanation",
            "field-provider-hwinfo",
            "hint-provider-hwinfo",
            "field-provider-lhm",
            "hint-provider-lhm",
            "field-lhm-url",
            "hint-lhm-loopback",
            "lhm-url-loopback-error",
            "section-sources-status",
            "provider-status-disabled",
            "provider-status-not-running",
            "provider-status-stale",
            "provider-status-error",
            "provider-age-now",
            "sources-no-providers-hint",
            "validation-lhm-url-loopback",
        ];
        for lang in ["uk", "en", "ru"] {
            let i18n = SettingsI18n::new(lang);
            for key in plain_keys {
                let text = i18n.t(key);
                assert_ne!(text, key, "missing i18n key `{key}` in locale `{lang}`");
                assert!(!text.trim().is_empty());
            }
            let conn = i18n.t_args("provider-status-connected", &[("count", "12")]);
            assert!(conn.contains("12"));
            let err_detail = i18n.t_args("provider-status-error-detail", &[("detail", "timeout")]);
            assert!(err_detail.contains("timeout"));
            let age_secs = i18n.t_args("provider-age-secs", &[("secs", "5")]);
            assert!(age_secs.contains('5'));
            let age_mins = i18n.t_args("provider-age-mins", &[("mins", "3")]);
            assert!(age_mins.contains('3'));
        }
    }

    #[test]
    fn pawnio_keys_exist_in_uk_en_ru() {
        let plain_keys = [
            "section-pawnio",
            "pawnio-explanation",
            "field-provider-pawnio",
            "hint-provider-pawnio",
            "pawnio-state-disabled",
            "pawnio-state-not-installed",
            "pawnio-state-stopped",
            "pawnio-state-starting",
            "pawnio-state-running",
            "pawnio-state-denied",
            "pawnio-state-error",
            "pawnio-not-installed",
            "btn-pawnio-enable",
            "btn-pawnio-stop",
            "pawnio-warning",
            "pawnio-manual-install",
        ];
        for lang in ["uk", "en", "ru"] {
            let i18n = SettingsI18n::new(lang);
            for key in plain_keys {
                let text = i18n.t(key);
                assert_ne!(text, key, "missing i18n key `{key}` in locale `{lang}`");
                assert!(
                    !text.trim().is_empty(),
                    "empty i18n key `{key}` in `{lang}`"
                );
            }
            let versions = i18n.t_args(
                "pawnio-versions",
                &[("pawnio", "2.1.0"), ("helper", "0.1.0")],
            );
            assert!(
                versions.contains("2.1.0"),
                "pawnio version missing: {versions}"
            );
            assert!(
                versions.contains("0.1.0"),
                "helper version missing: {versions}"
            );
            let temp = i18n.t_args("pawnio-temperature", &[("value", "72 °C")]);
            assert!(temp.contains("72 °C"), "temperature missing: {temp}");
        }
    }

    #[test]
    fn sources_update_keys_exist_in_uk_en_ru() {
        let plain_keys = [
            "field-lhm-auto-update",
            "hint-lhm-auto-update",
            "section-sources-updates",
            "sources-updates-explanation",
            "update-installed",
            "update-latest",
            "update-kind-managed",
            "update-kind-external",
            "update-kind-driver",
            "update-kind-none",
            "update-state-up-to-date",
            "update-state-available",
            "update-state-ready-hint",
            "update-state-updating",
            "update-state-unknown",
            "btn-official-releases",
            "btn-official-site",
        ];
        for lang in ["uk", "en", "ru"] {
            let i18n = SettingsI18n::new(lang);
            for key in plain_keys {
                let text = i18n.t(key);
                assert_ne!(text, key, "missing i18n key `{key}` in locale `{lang}`");
                assert!(!text.trim().is_empty());
            }
            let ready = i18n.t_args("update-state-ready", &[("path", r"C:\LHM\staged.exe")]);
            assert!(ready.contains(r"C:\LHM\staged.exe"));
            let err = i18n.t_args("update-state-error", &[("error", "timeout")]);
            assert!(err.contains("timeout"));
        }
    }
}
