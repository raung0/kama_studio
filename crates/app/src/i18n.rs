use std::{
    collections::{BTreeSet, HashMap},
    sync::{Mutex, OnceLock},
};

use anyhow::{Context, Result};
use i18n_embed::{
    fluent::{fluent_language_loader, FluentLanguageLoader},
    DesktopLanguageRequester,
};
use rust_embed::RustEmbed;
use unic_langid::LanguageIdentifier;

#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Localizations;

pub(crate) fn initialize(language: Option<&str>) -> Result<()> {
    set_language(language)
}

pub(crate) fn set_language(language: Option<&str>) -> Result<()> {
    let requested_languages = match language {
        Some(language) => vec![language
            .parse::<LanguageIdentifier>()
            .with_context(|| format!("invalid language preference: {language}"))?],
        None => DesktopLanguageRequester::requested_languages(),
    };
    i18n_embed::select(loader(), &Localizations, &requested_languages)
        .context("select embedded application translations")?;
    *language_preference()
        .lock()
        .expect("language preference lock") = language.map(str::to_owned);
    Ok(())
}

pub(crate) fn preference() -> Option<String> {
    language_preference()
        .lock()
        .expect("language preference lock")
        .clone()
}

pub(crate) fn text(message_id: &str) -> String {
    loader().get(message_id)
}

pub(crate) fn text_with_name(message_id: &str, name: &str) -> String {
    loader().get_args(message_id, HashMap::from([("name", name)]))
}

#[derive(Clone)]
pub(crate) struct LanguageOption {
    pub(crate) language: Option<String>,
    pub(crate) label: String,
}

pub(crate) fn language_options() -> Vec<LanguageOption> {
    let mut languages = Localizations::iter()
        .filter_map(|path| path.split('/').next()?.parse::<LanguageIdentifier>().ok())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|language| {
            let language = language.to_string();
            LanguageOption {
                label: text(&format!("language-{language}")),
                language: Some(language),
            }
        })
        .collect::<Vec<_>>();
    languages.insert(
        0,
        LanguageOption {
            language: None,
            label: text("settings-system"),
        },
    );
    languages
}

fn loader() -> &'static FluentLanguageLoader {
    static LOADER: OnceLock<FluentLanguageLoader> = OnceLock::new();
    LOADER.get_or_init(|| fluent_language_loader!())
}

fn language_preference() -> &'static Mutex<Option<String>> {
    static PREFERENCE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    PREFERENCE.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_and_system_preferences_select_a_language() {
        initialize(Some("en-US")).unwrap();
        assert_eq!(text("menu-file"), "File");
        assert_eq!(i18n_embed_fl::fl!(loader(), "menu-file"), "File");
        initialize(Some("ro-RO")).unwrap();
        assert_eq!(text("menu-file"), "Fișier");
        initialize(None).unwrap();
        assert!(matches!(text("menu-file").as_str(), "File" | "Fișier"));
        assert!(!loader().current_languages().is_empty());
    }
}
