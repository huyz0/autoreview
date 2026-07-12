use autoreview_schema::AutoreviewConfig;
use std::path::Path;

/// Loads .autoreview/config.yaml if present, otherwise returns pure defaults —
/// a bare `autoreview diff` in a repo with no config file must still work.
pub fn load_config(config_path: &Path) -> anyhow::Result<AutoreviewConfig> {
    match std::fs::read_to_string(config_path) {
        Ok(text) => {
            let config: AutoreviewConfig = serde_yaml::from_str(&text)?;
            Ok(config)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(AutoreviewConfig::default()),
        Err(err) => Err(err.into()),
    }
}
