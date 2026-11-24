//! `spel generate-idl [<CRATE_DIR>] [--message <ENUM>] [--state <TYPE>]... [--events <MODULE>]
//! [--namespace <NAME>]`: argument parsing for the IDL generator in `spel-framework-core`, with
//! anything not given on the command line taken from the selected `spel.toml` program (its
//! `core` crate and its `[idl-gen]` table).

use spel_framework_core::idl_gen::GenOptions;
use std::path::{Path, PathBuf};

use crate::config::ProgramConfig;

pub const USAGE: &str = "Usage: spel generate-idl [<CRATE_DIR>] [--message <ENUM>] [--state <TYPE>]... \
                         [--events <MODULE>] [--namespace <NAME>]\n\
                         Values not given come from spel.toml: the program's `core` and its \
                         [idl-gen] table (select it with --program <name>).";

/// The arguments after `generate-idl`, before `spel.toml` fills the gaps.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct GenArgs {
    pub crate_dir: Option<PathBuf>,
    pub message: Option<String>,
    /// `--state`, repeatable: several make the IDL's `state_types`.
    pub states: Vec<String>,
    pub events: Option<String>,
    pub namespace: Option<String>,
}

/// Parse the arguments after `generate-idl`.
///
/// # Errors
/// An unknown flag, a flag without its value, or a second crate directory.
pub fn parse_args(args: &[String]) -> Result<GenArgs, String> {
    let mut parsed = GenArgs::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| {
            iter.next()
                .filter(|v| !v.starts_with("--"))
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--message" => parsed.message = Some(value("--message")?),
            "--state" => parsed.states.push(value("--state")?),
            "--events" => parsed.events = Some(value("--events")?),
            "--namespace" => parsed.namespace = Some(value("--namespace")?),
            flag if flag.starts_with('-') => return Err(format!("unknown option '{flag}'")),
            dir if parsed.crate_dir.is_none() => parsed.crate_dir = Some(PathBuf::from(dir)),
            extra => return Err(format!("unexpected argument '{extra}'")),
        }
    }
    Ok(parsed)
}

/// Complete `args` from a `spel.toml` program (`config_dir` is the file's directory, which its
/// paths are relative to). Each command-line value wins; `--state` replaces the configured list
/// rather than adding to it. A message enum, state type or events module neither names is left
/// for the crate's `#[message]` / `#[state]` / `#[event]` markers.
///
/// # Errors
/// If neither gives a crate directory.
pub fn resolve(
    args: GenArgs,
    config: Option<(&Path, &ProgramConfig)>,
) -> Result<GenOptions, String> {
    let gen = config
        .and_then(|(_, program)| program.idl_gen.clone())
        .unwrap_or_default();
    let crate_dir = args
        .crate_dir
        .or_else(|| {
            let (dir, program) = config?;
            Some(dir.join(program.core.as_ref()?))
        })
        .ok_or("missing <CRATE_DIR>: pass it, or set `core` on the spel.toml program")?;
    let message = args.message.or(gen.message);
    let states = if args.states.is_empty() {
        gen.state.map(|s| s.into_vec()).unwrap_or_default()
    } else {
        args.states
    };
    Ok(GenOptions {
        crate_dir,
        message,
        states,
        events: args.events.or(gen.events),
        namespace: args.namespace.or(gen.namespace),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{IdlGenConfig, OneOrMany};

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    fn program(core: Option<&str>, gen: Option<IdlGenConfig>) -> ProgramConfig {
        ProgramConfig {
            address: None,
            idl: None,
            binary: None,
            core: core.map(str::to_owned),
            idl_gen: gen,
        }
    }

    #[test]
    fn parses_every_flag() {
        let options = resolve(
            parse_args(&args(
                "programs/token/core --message Message --state TokenState --events event --namespace tok",
            ))
            .unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(options.crate_dir, PathBuf::from("programs/token/core"));
        assert_eq!(options.message.as_deref(), Some("Message"));
        assert_eq!(options.states, ["TokenState"]);
        assert_eq!(options.events.as_deref(), Some("event"));
        assert_eq!(options.namespace.as_deref(), Some("tok"));
    }

    #[test]
    fn state_repeats() {
        let parsed = parse_args(&args("core --message M --state A --state B")).unwrap();
        assert_eq!(parsed.states, ["A", "B"]);
    }

    #[test]
    fn message_is_left_to_the_marker() {
        let options = resolve(parse_args(&args("core")).unwrap(), None).unwrap();
        assert_eq!(options.message, None);
        assert!(options.states.is_empty());
    }

    #[test]
    fn crate_dir_is_required() {
        let err = resolve(parse_args(&args("--message M")).unwrap(), None).unwrap_err();
        assert!(err.contains("CRATE_DIR"), "{err}");
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

    #[test]
    fn spel_toml_fills_everything() {
        let gen = IdlGenConfig {
            message: Some("Message".into()),
            state: Some(OneOrMany::Many(vec!["A".into(), "B".into()])),
            events: Some("event".into()),
            namespace: None,
        };
        let program = program(Some("programs/oracle/core"), Some(gen));
        let options = resolve(GenArgs::default(), Some((Path::new("/proj"), &program))).unwrap();
        assert_eq!(options.crate_dir, PathBuf::from("/proj/programs/oracle/core"));
        assert_eq!(options.message.as_deref(), Some("Message"));
        assert_eq!(options.states, ["A", "B"]);
        assert_eq!(options.events.as_deref(), Some("event"));
    }

    #[test]
    fn flags_win_over_spel_toml() {
        let gen = IdlGenConfig {
            message: Some("Message".into()),
            state: Some(OneOrMany::Many(vec!["A".into(), "B".into()])),
            events: Some("event".into()),
            namespace: Some("oracle".into()),
        };
        let program = program(Some("core"), Some(gen));
        let parsed = parse_args(&args("other/core --state C --namespace o2")).unwrap();
        let options = resolve(parsed, Some((Path::new("/proj"), &program))).unwrap();
        assert_eq!(options.crate_dir, PathBuf::from("other/core"));
        assert_eq!(options.states, ["C"], "--state replaces the configured list");
        assert_eq!(options.namespace.as_deref(), Some("o2"));
        assert_eq!(options.message.as_deref(), Some("Message"));
    }

    #[test]
    fn a_program_without_core_still_needs_a_crate_dir() {
        let program = program(None, None);
        let err = resolve(
            parse_args(&args("--message M")).unwrap(),
            Some((Path::new("/proj"), &program)),
        )
        .unwrap_err();
        assert!(err.contains("`core`"), "{err}");
    }
}
