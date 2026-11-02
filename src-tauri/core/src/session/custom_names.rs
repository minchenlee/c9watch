use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct CustomNames {
    pub names: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct CustomTitles {
    pub titles: HashMap<String, String>,
}

impl CustomNames {
    pub fn load() -> Self {
        let path = Self::get_path();
        if let Ok(content) = fs::read_to_string(path) {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::get_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let content = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, content).map_err(|e| e.to_string())
    }

    fn get_path() -> PathBuf {
        let home = dirs::home_dir().expect("Failed to get home directory");
        home.join(".claude").join("session-monitor-names.json")
    }

    pub fn get(&self, session_id: &str) -> Option<&String> {
        self.names.get(session_id)
    }

    pub fn set(&mut self, session_id: String, name: String) {
        self.names.insert(session_id, name);
    }
}

impl CustomTitles {
    pub fn load() -> Self {
        let path = Self::get_path();
        if let Ok(content) = fs::read_to_string(path) {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::get_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let content = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, content).map_err(|e| e.to_string())
    }

    fn get_path() -> PathBuf {
        let home = dirs::home_dir().expect("Failed to get home directory");
        home.join(".claude").join("session-monitor-titles.json")
    }

    pub fn get(&self, session_id: &str) -> Option<&String> {
        self.titles.get(session_id)
    }

    pub fn set(&mut self, session_id: String, title: String) {
        self.titles.insert(session_id, title);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_names_get_missing_key() {
        let names = CustomNames::default();
        assert_eq!(names.get("nonexistent-session-id"), None);
    }

    #[test]
    fn test_custom_names_set_and_get() {
        let mut names = CustomNames::default();
        names.set("session-123".to_string(), "My Project".to_string());
        assert_eq!(names.get("session-123"), Some(&"My Project".to_string()));
    }

    #[test]
    fn test_custom_names_overwrite() {
        let mut names = CustomNames::default();
        names.set("session-123".to_string(), "Old Name".to_string());
        names.set("session-123".to_string(), "New Name".to_string());
        assert_eq!(names.get("session-123"), Some(&"New Name".to_string()));
    }

    #[test]
    fn test_custom_names_multiple_sessions() {
        let mut names = CustomNames::default();
        names.set("session-a".to_string(), "Project A".to_string());
        names.set("session-b".to_string(), "Project B".to_string());
        assert_eq!(names.get("session-a"), Some(&"Project A".to_string()));
        assert_eq!(names.get("session-b"), Some(&"Project B".to_string()));
        assert_eq!(names.get("session-c"), None);
    }

    #[test]
    fn test_custom_titles_get_missing_key() {
        let titles = CustomTitles::default();
        assert_eq!(titles.get("nonexistent-session-id"), None);
    }

    #[test]
    fn test_custom_titles_set_and_get() {
        let mut titles = CustomTitles::default();
        titles.set("session-123".to_string(), "Fix the bug".to_string());
        assert_eq!(titles.get("session-123"), Some(&"Fix the bug".to_string()));
    }

    #[test]
    fn test_custom_titles_overwrite() {
        let mut titles = CustomTitles::default();
        titles.set("session-123".to_string(), "Old Title".to_string());
        titles.set("session-123".to_string(), "New Title".to_string());
        assert_eq!(titles.get("session-123"), Some(&"New Title".to_string()));
    }

    #[test]
    fn test_custom_titles_multiple_sessions() {
        let mut titles = CustomTitles::default();
        titles.set("session-a".to_string(), "Fix the bug".to_string());
        titles.set("session-b".to_string(), "Add feature".to_string());
        assert_eq!(titles.get("session-a"), Some(&"Fix the bug".to_string()));
        assert_eq!(titles.get("session-b"), Some(&"Add feature".to_string()));
        assert_eq!(titles.get("session-c"), None);
    }
}
