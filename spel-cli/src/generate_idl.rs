//! `spel generate-idl <CRATE_DIR> --message <ENUM> [--state <TYPE>] [--events <MODULE>]
//! [--namespace <NAME>]`: argument parsing for the IDL generator in `spel-framework-core`.

use spel_framework_core::idl_gen::GenOptions;
use std::path::PathBuf;

pub const USAGE: &str = "Usage: spel generate-idl <CRATE_DIR> --message <ENUM> [--state <TYPE>] \
                         [--events <MODULE>] [--namespace <NAME>]";

/// Parse the arguments after `generate-idl`.
///
/// # Errors
/// A missing crate directory or `--message`, an unknown flag, or a flag without its value.
pub fn parse_args(args: &[String]) -> Result<GenOptions, String> {
    let mut options = GenOptions::default();
    let mut crate_dir: Option<PathBuf> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| {
            iter.next()
                .filter(|v| !v.starts_with("--"))
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--message" => options.message = value("--message")?,
            "--state" => options.state = Some(value("--state")?),
            "--events" => options.events = Some(value("--events")?),
            "--namespace" => options.namespace = Some(value("--namespace")?),
            flag if flag.starts_with('-') => return Err(format!("unknown option '{flag}'")),
            dir if crate_dir.is_none() => crate_dir = Some(PathBuf::from(dir)),
            extra => return Err(format!("unexpected argument '{extra}'")),
        }
    }
    options.crate_dir = crate_dir.ok_or("missing <CRATE_DIR>")?;
    if options.message.is_empty() {
        return Err("missing --message <ENUM>".into());
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_every_flag() {
        let options = parse_args(&args(
            "programs/token/core --message Message --state TokenState --events event --namespace tok",
        ))
        .unwrap();
        assert_eq!(options.crate_dir, PathBuf::from("programs/token/core"));
        assert_eq!(options.message, "Message");
        assert_eq!(options.state.as_deref(), Some("TokenState"));
        assert_eq!(options.events.as_deref(), Some("event"));
        assert_eq!(options.namespace.as_deref(), Some("tok"));
    }

    #[test]
    fn message_is_required() {
        assert!(parse_args(&args("core")).unwrap_err().contains("--message"));
    }

    #[test]
    fn crate_dir_is_required() {
        assert!(parse_args(&args("--message M"))
            .unwrap_err()
            .contains("CRATE_DIR"));
    }

    #[test]
    fn flag_without_value_is_refused() {
        assert!(parse_args(&args("core --message --state S"))
            .unwrap_err()
            .contains("--message needs a value"));
    }

    #[test]
    fn unknown_flag_is_refused() {
        assert!(parse_args(&args("core --message M --idl x")).is_err());
    }
}
