use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    Turkish,
    English,
}

impl Language {
    pub fn label(self) -> &'static str {
        match self {
            Self::Turkish => "Türkçe",
            Self::English => "English",
        }
    }
}

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn set_language(language: Language) {
    ENGLISH.store(language == Language::English, Ordering::Relaxed);
}
pub fn is_english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

pub fn choose<T>(turkish: T, english: T) -> T {
    if is_english() { english } else { turkish }
}

fn catalog() -> &'static HashMap<String, String> {
    static CATALOG: OnceLock<HashMap<String, String>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../assets/locales/en.json"))
            .expect("valid embedded English catalog")
    })
}

pub fn text(source: &str) -> &str {
    if is_english() {
        catalog().get(source).map(String::as_str).unwrap_or(source)
    } else {
        source
    }
}

struct MessageTemplate {
    turkish: regex::Regex,
    english: regex::Regex,
    turkish_parts: Vec<String>,
    english_parts: Vec<String>,
    specificity: usize,
}

fn templates() -> &'static Vec<MessageTemplate> {
    static TEMPLATES: OnceLock<Vec<MessageTemplate>> = OnceLock::new();
    TEMPLATES.get_or_init(|| {
        let placeholders = regex::Regex::new(r"\{[^{}]*\}").unwrap();
        let parts = |s: &str| placeholders.split(s).map(str::to_owned).collect::<Vec<_>>();
        let pattern = |p: &[String]| {
            regex::Regex::new(&format!(
                "(?s)^{}$",
                p.iter()
                    .map(|s| regex::escape(s))
                    .collect::<Vec<_>>()
                    .join("(.*?)")
            ))
            .unwrap()
        };
        let mut result = Vec::new();
        for (tr, en) in catalog() {
            if tr == en || !placeholders.is_match(tr) {
                continue;
            }
            let turkish_parts = parts(tr);
            let english_parts = parts(en);
            result.push(MessageTemplate {
                turkish: pattern(&turkish_parts),
                english: pattern(&english_parts),
                specificity: turkish_parts.iter().map(String::len).sum(),
                turkish_parts,
                english_parts,
            });
        }
        result.sort_by_key(|r| std::cmp::Reverse(r.specificity));
        result
    })
}

// Shared views may predate a language change. Translate their message template,
// preserving captured file names, paths and numeric values verbatim.
fn display_for(language: Language, source: &str) -> String {
    let english = language == Language::English;
    if english {
        if let Some(s) = catalog().get(source) {
            return s.clone();
        }
        if catalog().values().any(|s| s == source) {
            return source.into();
        }
    } else {
        if catalog().contains_key(source) {
            return source.into();
        }
        if let Some((s, _)) = catalog().iter().find(|(_, en)| *en == source) {
            return s.clone();
        }
    }
    for rule in templates() {
        let (from, to) = if english {
            (&rule.turkish, &rule.english_parts)
        } else {
            (&rule.english, &rule.turkish_parts)
        };
        if let Some(captures) = from.captures(source) {
            let mut result = to[0].clone();
            for (i, part) in to.iter().enumerate().skip(1) {
                result.push_str(captures.get(i).unwrap().as_str());
                result.push_str(part);
            }
            return result;
        }
    }
    source.into()
}

pub fn display(source: &str) -> String {
    display_for(
        if is_english() {
            Language::English
        } else {
            Language::Turkish
        },
        source,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn translations_preserve_format_arguments() {
        let placeholders = regex::Regex::new(r"\{[^{}]*\}").unwrap();
        for (source, english) in catalog() {
            assert!(!english.trim().is_empty());
            let a: Vec<_> = placeholders.find_iter(source).map(|m| m.as_str()).collect();
            let b: Vec<_> = placeholders
                .find_iter(english)
                .map(|m| m.as_str())
                .collect();
            assert_eq!(a, b, "format arguments must remain in order: {source}");
        }
        assert_eq!(catalog().get("Ayarlar").unwrap(), "Settings");
        assert_eq!(catalog().get("Güncel").unwrap(), "Up to date");
    }

    #[test]
    fn language_round_trips_and_old_configs_default_to_turkish() {
        for language in [Language::Turkish, Language::English] {
            assert_eq!(
                serde_json::from_str::<Language>(&serde_json::to_string(&language).unwrap())
                    .unwrap(),
                language
            );
        }
        let cfg: crate::config::Config = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.language, Language::Turkish);
        assert!(cfg.host.is_empty());
        assert!(cfg.host_fingerprint.is_empty());
        assert!(!cfg.auto_start);
    }

    #[test]
    fn existing_view_messages_switch_language_without_changing_file_names() {
        assert_eq!(display_for(Language::English, "Güncel ✓"), "Up to date ✓");
        assert_eq!(
            display_for(Language::English, "İndirilecek (~42 GB, ~2h)"),
            "Pending download (~42 GB, ~2h)"
        );
        assert_eq!(
            display_for(
                Language::Turkish,
                "Up to date ✓ · next scheduled 03.11 12:00"
            ),
            "Güncel ✓ · sonraki plan 03.11 12:00"
        );
        assert_eq!(
            display_for(Language::English, "Sunucu hash'i bekleniyor"),
            "Waiting for server hash"
        );
        assert_eq!(
            display_for(Language::English, "VM_ÇARŞI.vbk"),
            "VM_ÇARŞI.vbk"
        );
    }
}
