//! Lo mínimo de `config` que necesita `theme.rs`: temas por nombre.

#[derive(Clone, PartialEq)]
pub struct CustomTheme {
    pub name: String,
    pub dark: bool,
    pub colors: [[u8; 3]; 9],
}

#[derive(Default)]
pub struct Config {
    pub theme_custom: String,
}

impl Config {
    pub fn theme_named(&self, name: &str) -> Option<CustomTheme> {
        if name.is_empty() {
            return None;
        }
        crate::theme::presets().iter().find(|t| t.name == name).cloned()
    }
}
