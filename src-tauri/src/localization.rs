use crate::models::LanguagePreference;

pub fn uses_simplified_chinese(preference: LanguagePreference) -> bool {
    match preference {
        LanguagePreference::English => false,
        LanguagePreference::SimplifiedChinese => true,
        LanguagePreference::System => system_uses_chinese(),
    }
}

pub fn system_language() -> &'static str {
    if system_uses_chinese() {
        "zh-CN"
    } else {
        "en"
    }
}

#[cfg(target_os = "windows")]
fn system_uses_chinese() -> bool {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;

    let mut locale = [0u16; 85];
    let length = unsafe { GetUserDefaultLocaleName(&mut locale) };
    if length <= 1 {
        return false;
    }
    String::from_utf16_lossy(&locale[..length as usize - 1])
        .to_ascii_lowercase()
        .starts_with("zh")
}

#[cfg(not(target_os = "windows"))]
fn system_uses_chinese() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("zh"))
}

#[cfg(test)]
mod tests {
    use super::uses_simplified_chinese;
    use crate::models::LanguagePreference;

    #[test]
    fn explicit_language_preferences_do_not_depend_on_the_platform_locale() {
        assert!(!uses_simplified_chinese(LanguagePreference::English));
        assert!(uses_simplified_chinese(
            LanguagePreference::SimplifiedChinese
        ));
    }
}
