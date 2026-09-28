use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn is_english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

pub fn toggle() {
    ENGLISH.fetch_xor(true, Ordering::Relaxed);
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
}
