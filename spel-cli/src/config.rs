//! `spel.toml` config file discovery and parsing.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const CONFIG_FILENAME: &str = "spel.toml";

#[derive(Debug, Deserialize)]
pub struct SpelConfig {
    /// Single program shorthand: `[program]`
    pub program: Option<ProgramConfig>,
    /// Named programs: `[programs.<name>]`
    pub programs: Option<HashMap<String, ProgramConfig>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramConfig {
    pub idl: Option<String>,
    pub binary: Option<String>,
}

impl SpelConfig {
    /// Walk up from `start_dir` looking for `spel.toml`.
    /// Returns `None` if no config file is found.
    pub fn discover(start_dir: &Path) -> Option<(PathBuf, SpelConfig)> {
        let mut dir = start_dir.to_path_buf();
        loop {
            let candidate = dir.join(CONFIG_FILENAME);
            if candidate.is_file() {
                match Self::load(&candidate) {
                    Ok(config) => return Some((candidate, config)),
                    Err(e) => {
                        eprintln!("❌ Error reading {}: {}", candidate.display(), e);
                        std::process::exit(1);
                    }
                }
            }
            if !dir.pop() {
                return None;
            }
        }
    }

    /// Load and parse a `spel.toml` file at the given path.
    pub fn load(path: &Path) -> Result<SpelConfig, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read '{}': {}", path.display(), e))?;
        let config: SpelConfig = toml::from_str(&content)
            .map_err(|e| format!("invalid TOML in '{}': {}", path.display(), e))?;
        config.validate(path)?;
        Ok(config)
    }

    /// Check that `[program]` and `[programs]` are not both present.
    fn validate(&self, path: &Path) -> Result<(), String> {
        let has_program = self.program.is_some();
        let has_programs = self.programs.as_ref().is_some_and(|p| !p.is_empty());
        if has_program && has_programs {
            return Err(format!(
                "invalid config in '{}': [program] and [programs] are mutually exclusive. \
                 Use [program] for single-program projects or [programs.<name>] for multi-program.",
                path.display()
            ));
        }
        Ok(())
    }

    /// Resolve which program config to use.
    ///
    /// - `name = Some("x")` → look up `[programs.x]`
    /// - `name = None` + `[program]` exists → use it
    /// - `name = None` + exactly one `[programs.x]` → use it
    /// - `name = None` + multiple `[programs]` → error
    pub fn resolve_program(&self, name: Option<&str>) -> Result<&ProgramConfig, String> {
        if let Some(name) = name {
            // Explicit name: must be in [programs.<name>]
            if let Some(programs) = &self.programs {
                if let Some(cfg) = programs.get(name) {
                    return Ok(cfg);
                }
                let available: Vec<&str> = programs.keys().map(|s| s.as_str()).collect();
                return Err(format!(
                    "program '{}' not found in spel.toml. Available: {}",
                    name,
                    available.join(", ")
                ));
            }
            return Err(format!(
                "program '{}' not found: spel.toml has no [programs] section",
                name
            ));
        }

        // No name given: auto-resolve
        if let Some(ref cfg) = self.program {
            return Ok(cfg);
        }
        if let Some(programs) = &self.programs {
            if programs.len() == 1 {
                return Ok(programs.values().next().unwrap());
            }
            if programs.is_empty() {
                return Err("spel.toml has no programs defined".to_string());
            }
            let available: Vec<&str> = programs.keys().map(|s| s.as_str()).collect();
            return Err(format!(
                "multiple programs in spel.toml — specify one with --program <name>. Available: {}",
                available.join(", ")
            ));
        }
        Err("spel.toml has no [program] or [programs] section".to_string())
    }

    /// Check if a name matches a program entry in the config.
    pub fn has_program(&self, name: &str) -> bool {
        self.programs
            .as_ref()
            .is_some_and(|p| p.contains_key(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_single_program() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("spel.toml");
        fs::write(&config_path, r#"
[program]
idl = "my-project-idl.json"
binary = "target/my_project.bin"
"#).unwrap();

        let config = SpelConfig::load(&config_path).unwrap();
        let prog = config.resolve_program(None).unwrap();
        assert_eq!(prog.idl.as_deref(), Some("my-project-idl.json"));
        assert_eq!(prog.binary.as_deref(), Some("target/my_project.bin"));
    }

    #[test]
    fn parse_multi_program() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("spel.toml");
        fs::write(&config_path, r#"
[programs.game]
idl = "game-idl.json"
binary = "target/game.bin"

[programs.nft]
idl = "nft-idl.json"
binary = "target/nft.bin"
"#).unwrap();

        let config = SpelConfig::load(&config_path).unwrap();

        let game = config.resolve_program(Some("game")).unwrap();
        assert_eq!(game.idl.as_deref(), Some("game-idl.json"));

        let nft = config.resolve_program(Some("nft")).unwrap();
        assert_eq!(nft.idl.as_deref(), Some("nft-idl.json"));
    }

    #[test]
    fn multi_program_auto_select_single() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("spel.toml");
        fs::write(&config_path, r#"
[programs.only_one]
idl = "only.json"
binary = "only.bin"
"#).unwrap();

        let config = SpelConfig::load(&config_path).unwrap();
        let prog = config.resolve_program(None).unwrap();
        assert_eq!(prog.idl.as_deref(), Some("only.json"));
    }

    #[test]
    fn multi_program_no_name_errors() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("spel.toml");
        fs::write(&config_path, r#"
[programs.a]
idl = "a.json"

[programs.b]
idl = "b.json"
"#).unwrap();

        let config = SpelConfig::load(&config_path).unwrap();
        let err = config.resolve_program(None).unwrap_err();
        assert!(err.contains("--program <name>"));
        assert!(err.contains("a"));
        assert!(err.contains("b"));
    }

    #[test]
    fn unknown_program_name_errors() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("spel.toml");
        fs::write(&config_path, r#"
[programs.game]
idl = "game.json"
"#).unwrap();

        let config = SpelConfig::load(&config_path).unwrap();
        let err = config.resolve_program(Some("nope")).unwrap_err();
        assert!(err.contains("nope"));
        assert!(err.contains("game"));
    }

    #[test]
    fn mutual_exclusion_errors() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("spel.toml");
        fs::write(&config_path, r#"
[program]
idl = "single.json"

[programs.multi]
idl = "multi.json"
"#).unwrap();

        let err = SpelConfig::load(&config_path).unwrap_err();
        assert!(err.contains("mutually exclusive"));
    }

    #[test]
    fn has_program_check() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("spel.toml");
        fs::write(&config_path, r#"
[programs.game]
idl = "game.json"
"#).unwrap();

        let config = SpelConfig::load(&config_path).unwrap();
        assert!(config.has_program("game"));
        assert!(!config.has_program("nope"));
    }

    #[test]
    fn parse_empty_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("spel.toml");
        fs::write(&config_path, "").unwrap();

        let config = SpelConfig::load(&config_path).unwrap();
        assert!(config.resolve_program(None).is_err());
    }

    #[test]
    fn parse_partial_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("spel.toml");
        fs::write(&config_path, r#"
[program]
idl = "foo.json"
"#).unwrap();

        let config = SpelConfig::load(&config_path).unwrap();
        let prog = config.resolve_program(None).unwrap();
        assert_eq!(prog.idl.as_deref(), Some("foo.json"));
        assert_eq!(prog.binary.as_deref(), None);
    }

    #[test]
    fn parse_malformed_toml_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("spel.toml");
        fs::write(&config_path, "this is not valid toml [[[").unwrap();

        let result = SpelConfig::load(&config_path);
        assert!(result.is_err());
    }

    #[test]
    fn discover_walks_up() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("spel.toml");
        fs::write(&config_path, r#"
[program]
idl = "found.json"
"#).unwrap();

        let subdir = dir.path().join("sub/deep");
        fs::create_dir_all(&subdir).unwrap();

        let result = SpelConfig::discover(&subdir);
        assert!(result.is_some());
        let (path, config) = result.unwrap();
        assert_eq!(path, config_path);
        let prog = config.resolve_program(None).unwrap();
        assert_eq!(prog.idl.as_deref(), Some("found.json"));
    }

    #[test]
    fn discover_returns_none_when_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let result = SpelConfig::discover(dir.path());
        assert!(result.is_none());
    }
}
