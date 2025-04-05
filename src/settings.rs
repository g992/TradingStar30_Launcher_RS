use directories_next::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;

pub const CONFIG_FILE_NAME: &str = "launcher_settings.json"; // Сделаем публичной, может понадобиться

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppSettings {
    pub executable_path: Option<PathBuf>, // Поля делаем публичными
    pub api_key: String,
    pub last_pid: Option<u32>,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            executable_path: None,
            api_key: String::new(),
            last_pid: None,
        }
    }
}

pub fn get_config_path() -> Option<PathBuf> {
    ProjectDirs::from("com", "TradingStar", "TradingStar3Launcher").map(|dirs| {
        let config_dir = dirs.config_dir();
        config_dir.join(CONFIG_FILE_NAME)
    })
}

pub async fn load_settings(path: Option<PathBuf>) -> Result<AppSettings, String> {
    let path = path.ok_or_else(|| "Не удалось определить путь к конфигурации".to_string())?;
    if !path.exists() {
        return Ok(AppSettings::default());
    }
    let content = fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Ошибка чтения файла конфигурации {:?}: {}", path, e))?;
    serde_json::from_str(&content)
        .map_err(|e| format!("Ошибка парсинга файла конфигурации {:?}: {}", path, e))
}

pub async fn save_settings(path: Option<PathBuf>, settings: AppSettings) -> Result<(), String> {
    let path = path.ok_or_else(|| "Не удалось определить путь к конфигурации".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Не удалось создать директорию {:?}: {}", parent, e))?;
    }
    let content = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("Ошибка сериализации настроек: {}", e))?;
    let mut file = fs::File::create(&path).await.map_err(|e| {
        format!(
            "Не удалось создать/открыть файл конфигурации {:?}: {}",
            path, e
        )
    })?;
    file.write_all(content.as_bytes())
        .await
        .map_err(|e| format!("Не удалось записать в файл конфигурации {:?}: {}", path, e))?;
    Ok(())
}
