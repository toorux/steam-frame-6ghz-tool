use std::{fs, path::PathBuf, sync::atomic::{AtomicBool, Ordering}};
use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn is_english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

pub fn toggle() -> std::io::Result<()> {
    let english = !ENGLISH.fetch_xor(true, Ordering::Relaxed);
    save_preference(english)
}

pub fn initialize() {
    // Primary language ID 0x04 covers all Chinese UI-language variants.
    let system_english = unsafe { GetUserDefaultUILanguage() } & 0x03ff != 0x04;
    let saved = preference_path().ok().and_then(|path| fs::read(path).ok());
    let selected = choose_language(system_english, saved.as_deref());
    ENGLISH.store(selected, Ordering::Relaxed);
}

fn preference_path() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "LOCALAPPDATA"))?;
    Ok(PathBuf::from(base).join("SteamFrame6GHzTool").join("language.txt"))
}

fn parse_preference(bytes: &[u8]) -> Option<bool> {
    match bytes {
        b"en" => Some(true),
        b"zh" => Some(false),
        _ => None,
    }
}

fn choose_language(system_english: bool, saved: Option<&[u8]>) -> bool {
    saved.and_then(parse_preference).unwrap_or(system_english)
}

fn save_preference(english: bool) -> std::io::Result<()> {
    let path = preference_path()?;
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(path, if english { b"en" } else { b"zh" })
}

pub fn t<'a>(zh: &'a str, en: &'a str) -> &'a str {
    pick(is_english(), zh, en)
}

fn pick<'a>(english: bool, zh: &'a str, en: &'a str) -> &'a str {
    if english { en } else { zh }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_requested_language() {
        assert_eq!(pick(false, "中文", "English"), "中文");
        assert_eq!(pick(true, "中文", "English"), "English");
    }

    #[test]
    fn only_explicit_language_choices_override_system_default() {
        assert_eq!(parse_preference(b"en"), Some(true));
        assert_eq!(parse_preference(b"zh"), Some(false));
        assert_eq!(parse_preference(b""), None);
        assert_eq!(parse_preference(b"other"), None);
        assert!(choose_language(true, None));
        assert!(!choose_language(false, None));
        assert!(!choose_language(true, Some(b"zh")));
        assert!(choose_language(false, Some(b"en")));
    }
}
