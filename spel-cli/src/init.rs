//! `spel init`: scaffold an actor program for LEZ.
//!
//! The project is a counter: one message enum and one state type in a core crate, marked for
//! `spel generate-idl`; a handler in a program crate, unit-testable on the host; and a risc0
//! guest that runs the handler. Its `spel.toml` names the IDL, the binary and the core crate, so
//! `make build idl deploy` and then `spel send` / `spel inspect` need no further flags.

use std::fs;
use std::path::{Path, PathBuf};

pub const USAGE: &str = "Usage: spel init <PROJECT> [--lez-tag <TAG> | --lez-rev <REV>] \
     [--spel-tag <TAG> | --spel-rev <REV> | --spel-path <DIR>] [--spel-git <URL>]";

/// The LEZ tag a scaffold pins unless `--lez-tag` or `--lez-rev` says otherwise: the one this
/// spel links.
pub const DEFAULT_LEZ_TAG: &str = "v0.4.0-rc3";

const LEZ_GIT: &str = "https://github.com/logos-blockchain/logos-execution-zone.git";
const SPEL_GIT: &str = "https://github.com/logos-co/spel.git";

/// The commit this spel was built from (empty outside a git checkout), see `build.rs`.
const SPEL_GIT_REV: &str = env!("SPEL_GIT_REV");

/// Where a scaffold's dependencies come from.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct InitArgs {
    pub project: String,
    pub lez_tag: Option<String>,
    pub lez_rev: Option<String>,
    pub spel_tag: Option<String>,
    pub spel_rev: Option<String>,
    pub spel_path: Option<String>,
    pub spel_git: Option<String>,
}

/// Parse the arguments after `init`.
///
/// # Errors
/// An unknown flag, a flag without its value, two ways to name one dependency, or no project.
pub fn parse_args(args: &[String]) -> Result<InitArgs, String> {
    let mut parsed = InitArgs::default();
    let mut project = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| {
            iter.next()
                .filter(|v| !v.starts_with("--"))
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--lez-tag" => parsed.lez_tag = Some(value(arg)?),
            "--lez-rev" => parsed.lez_rev = Some(value(arg)?),
            "--spel-tag" => parsed.spel_tag = Some(value(arg)?),
            "--spel-rev" => parsed.spel_rev = Some(value(arg)?),
            "--spel-path" => parsed.spel_path = Some(value(arg)?),
            "--spel-git" => parsed.spel_git = Some(value(arg)?),
            flag if flag.starts_with('-') => return Err(format!("unknown option '{flag}'")),
            name if project.is_none() => project = Some(name.to_owned()),
            name => return Err(format!("one project at a time, got another: '{name}'")),
        }
    }
    if parsed.lez_tag.is_some() && parsed.lez_rev.is_some() {
        return Err("pass --lez-tag or --lez-rev, not both".into());
    }
    let spel_refs = [&parsed.spel_tag, &parsed.spel_rev, &parsed.spel_path]
        .iter()
        .filter(|r| r.is_some())
        .count();
    if spel_refs > 1 {
        return Err("pass one of --spel-tag, --spel-rev and --spel-path".into());
    }
    if parsed.spel_path.is_some() && parsed.spel_git.is_some() {
        return Err("--spel-git names a repository; --spel-path a local checkout. Pass one".into());
    }
    parsed.project = project.ok_or("which project? spel init <PROJECT>")?;
    Ok(parsed)
}

/// The names a project directory yields: `my-counter` is the project, `my_counter` the Rust and
/// spel.toml name.
fn names(project_dir: &Path) -> Result<(String, String), String> {
    let project = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("no project name in '{}'", project_dir.display()))?
        .to_owned();
    let snake = project.replace('-', "_");
    let valid = snake
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && snake
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err(format!(
            "'{project}' isn't a usable project name: use lowercase letters, digits, '-' and '_', \
             starting with a letter"
        ));
    }
    Ok((project, snake))
}

/// A Cargo git dependency selector for LEZ.
fn lez_dependency(args: &InitArgs) -> String {
    match (&args.lez_tag, &args.lez_rev) {
        (_, Some(rev)) => format!(r#"git = "{LEZ_GIT}", rev = "{rev}""#),
        (Some(tag), _) => format!(r#"git = "{LEZ_GIT}", tag = "{tag}""#),
        _ => format!(r#"git = "{LEZ_GIT}", tag = "{DEFAULT_LEZ_TAG}""#),
    }
}

/// The `spel-attrs` dependency: a local checkout, a given tag or revision, else the commit this
/// spel was built from.
///
/// # Errors
/// If `--spel-path` doesn't hold a spel checkout, or no revision is known.
fn spel_attrs_dependency(args: &InitArgs) -> Result<String, String> {
    if let Some(path) = &args.spel_path {
        let attrs = Path::new(path).join("spel-attrs");
        if !attrs.join("Cargo.toml").exists() {
            return Err(format!("--spel-path {path}: no spel-attrs crate in it"));
        }
        let absolute = attrs
            .canonicalize()
            .map_err(|e| format!("--spel-path {path}: {e}"))?;
        return Ok(format!(r#"path = "{}""#, absolute.display()));
    }
    let git = args.spel_git.as_deref().unwrap_or(SPEL_GIT);
    Ok(match (&args.spel_tag, &args.spel_rev) {
        (Some(tag), _) => format!(r#"git = "{git}", tag = "{tag}""#),
        (_, Some(rev)) => format!(r#"git = "{git}", rev = "{rev}""#),
        _ if !SPEL_GIT_REV.is_empty() => format!(r#"git = "{git}", rev = "{SPEL_GIT_REV}""#),
        _ => {
            return Err(
                "this spel wasn't built from a git checkout, so it can't pin its own commit: pass \
                 --spel-tag, --spel-rev or --spel-path"
                    .into(),
            )
        },
    })
}

/// Every file of a scaffold, as (path, content).
///
/// # Errors
/// As [`spel_attrs_dependency`].
pub fn scaffold(
    project: &str,
    snake: &str,
    args: &InitArgs,
) -> Result<Vec<(String, String)>, String> {
    let lez = lez_dependency(args);
    let spel_attrs = spel_attrs_dependency(args)?;
    let core = format!("{snake}_core");
    let program = format!("{snake}_program");
    let bin_dir = "guest/target/riscv32im-risc0-zkvm-elf/docker";

    let files = vec![
        (
            "Cargo.toml".to_owned(),
            format!(
                r#"[workspace]
members = ["{core}", "{program}"]
# The guest builds for the zkVM with its own toolchain: `make build`.
exclude = ["guest"]
resolver = "2"
"#
            ),
        ),
        (
            ".gitignore".to_owned(),
            "target/\nguest/target/\n".to_owned(),
        ),
        (
            "spel.toml".to_owned(),
            format!(
                r#"# {project}: a spel project. Paths are relative to this file.
#
# `address` is the program's header account, which `spel deploy` prints. It isn't derivable
# from the binary, and every deploy creates a new one; `make deploy` records it here.

[programs.{snake}]
# address = ""
idl    = "{snake}-idl.json"
binary = "{bin_dir}/{snake}.bin"
core   = "{core}"
"#
            ),
        ),
        (
            "Makefile".to_owned(),
            format!(
                r#"# {project}: build the guest, generate its IDL, deploy it.
#
#   make build                 the guest binary (needs the risc0 toolchain and Docker)
#   make idl                   {snake}-idl.json, from {core}
#   make deploy FEE_PAYER=…    deploy, and record the address in spel.toml
#   make test                  the handler's unit tests

SHELL := /bin/bash
SPEL ?= spel
FEE_PAYER ?=

.PHONY: help build idl deploy test

help: ## Show this help
	@grep -E '^[a-z]+:.*## ' $(MAKEFILE_LIST) | sed -E 's/:.*## /\t/'

build: ## Build the guest binary
	cargo risczero build --manifest-path guest/Cargo.toml
	@echo "✅ {bin_dir}/{snake}.bin"

idl: ## Generate the IDL from the core crate (spel.toml names everything)
	$(SPEL) generate-idl > {snake}-idl.json
	@echo "✅ {snake}-idl.json"

deploy: ## Deploy the guest and record its address in spel.toml
	@test -n "$(FEE_PAYER)" || {{ echo "make deploy FEE_PAYER=<funded wallet account>"; exit 1; }}
	@ADDR=$$($(SPEL) deploy --fee-payer $(FEE_PAYER)) && \
	  sed -i.bak -E 's|^#? *address *=.*|address = "'"$$ADDR"'"|' spel.toml && rm -f spel.toml.bak && \
	  echo "✅ deployed at $$ADDR (recorded in spel.toml)"

test: ## Run the handler's unit tests
	cargo test
"#
            ),
        ),
        (
            "README.md".to_owned(),
            format!(
                r#"# {project}

An actor program for LEZ, scaffolded by `spel init`: a counter. Each account that the program
has state on keeps its own count.

| path | contents |
| --- | --- |
| `{core}/` | the message enum and the state type, shared by the guest and clients |
| `{program}/` | the handler: one message for one actor, `(account, {snake})` |
| `guest/` | the zkVM binary that runs the handler |
| `spel.toml` | the program's IDL, binary, core crate and (after deploying) address |

## Build, generate the IDL, deploy

```bash
make build                      # needs the risc0 toolchain (rzup) and Docker
make idl                        # spel generate-idl, no flags: the core crate marks its types
make deploy FEE_PAYER=<account> # a funded account in the wallet at LEE_WALLET_HOME_DIR
```

## Send and inspect

Accounts are labels in LEZ's wallet (`wallet account new public --label alice`).

```bash
spel send --root alice --signer alice -- increment --by 2
spel inspect alice@{snake}      # {{"count": "2"}}
spel send --root alice --signer alice -- reset
```

`increment` and `reset` require the account's authority, so the account signs (`--signer`).

## Privately

The same messages to a private account (`wallet account new private --label carol`) are sent
as a privacy-preserving transaction: the wallet runs the handler locally and proves it, and only
the proof and encrypted notes are published. The account authorizes with the wallet's key and
pays the fee in the proof.

```bash
spel send --root carol --signer carol -- increment --by 5
spel inspect carol@{snake}      # read from the wallet's decrypted copy
```

## Changing the program

- Messages and state are Borsh: add new message variants at the end, never reorder or edit
  existing ones, since a variant's position is its tag.
- After a change, `make build idl` and deploy again (a new deploy is a new address).
- Events: mark the program's event trait `#[cfg_attr(feature = "idl", spel_attrs::event)]`, and
  `make idl` lists its impls.
"#
            ),
        ),
        // Core crate
        (
            format!("{core}/Cargo.toml"),
            format!(
                r#"[package]
name = "{core}"
version = "0.1.0"
edition = "2021"

[dependencies]
borsh = {{ version = "1.5", features = ["derive"] }}
# The inert #[message] / #[state] markers `spel generate-idl` reads from the source. Optional:
# the generator finds them either way, and the guest never builds this crate.
spel-attrs = {{ {spel_attrs}, optional = true }}

[features]
# `cargo check --features idl` checks the markers compile.
idl = ["dep:spel-attrs"]
"#
            ),
        ),
        (
            format!("{core}/src/lib.rs"),
            format!(
                r#"//! Types shared by the {project} program and its clients: the message it accepts and the
//! state it keeps on each account.
//!
//! Borsh encodes an enum variant as a leading tag byte, so the message enum is append-only: add
//! variants at the end, never reorder or edit existing ones.

use borsh::{{BorshDeserialize, BorshSerialize}};

/// What the program accepts.
#[cfg_attr(feature = "idl", spel_attrs::message)]
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Message {{
    /// Add `by` to the receiving account's count. The account must authorize.
    Increment {{ by: u64 }},
    /// Set the receiving account's count back to zero. The account must authorize.
    Reset,
}}

/// The program's state on one account. An account it has never written to has none, which
/// counts as zero.
#[cfg_attr(feature = "idl", spel_attrs::state)]
#[derive(Debug, Clone, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CounterState {{
    pub count: u64,
}}
"#
            ),
        ),
        // Program crate
        (
            format!("{program}/Cargo.toml"),
            format!(
                r#"[package]
name = "{program}"
version = "0.1.0"
edition = "2021"

[dependencies]
# Without the `host` feature: the guest builds this crate too.
lee_core = {{ {lez} }}
{core} = {{ path = "../{core}" }}
borsh = {{ version = "1.5", features = ["derive"] }}

[dev-dependencies]
# `host` derives `Debug` and `PartialEq` on `Response`, for assertions.
lee_core = {{ {lez}, features = ["host"] }}
"#
            ),
        ),
        (
            format!("{program}/src/lib.rs"),
            format!(
                r#"//! The {project} program. Each run handles one message for one actor: the receiving
//! account's state under this program.

use lee_core::program::{{ReceiveInput, Response}};
use {core}::{{CounterState, Message}};

/// The handler the guest runs.
///
/// # Panics
/// If the receiving account doesn't authorize, or the count would overflow. A panic refuses the
/// message: the wallet's simulation reports it, and nothing is sent.
pub fn handle_message(input: &ReceiveInput, message: Message) -> Response {{
    assert!(input.is_authorized, "the counter's account must authorize");
    let mut state = if input.pre_state.is_empty() {{
        CounterState::default()
    }} else {{
        borsh::from_slice(&input.pre_state).expect("counter state must decode")
    }};
    match message {{
        Message::Increment {{ by }} => {{
            state.count = state.count.checked_add(by).expect("count overflow");
        }},
        Message::Reset => state.count = 0,
    }}
    Response::set_state(borsh::to_vec(&state).expect("borsh serialization is infallible"))
}}

#[cfg(test)]
mod tests {{
    use super::*;
    use lee_core::account::{{AccountId, Actor, ActorState}};

    fn run(is_authorized: bool, count: Option<u64>, message: Message) -> Response {{
        let input = ReceiveInput {{
            receiver: Actor::new(AccountId::new([1; 32]), AccountId::new([9; 32])),
            from: None,
            is_authorized,
            pre_state: ActorState::from(
                count
                    .map(|count| borsh::to_vec(&CounterState {{ count }}).unwrap())
                    .unwrap_or_default(),
            ),
            message: borsh::to_vec(&message).unwrap(),
        }};
        handle_message(&input, message)
    }}

    fn counted(count: u64) -> Response {{
        Response::set_state(borsh::to_vec(&CounterState {{ count }}).unwrap())
    }}

    #[test]
    fn increment_adds_to_the_count() {{
        assert_eq!(run(true, None, Message::Increment {{ by: 2 }}), counted(2));
        assert_eq!(run(true, Some(2), Message::Increment {{ by: 3 }}), counted(5));
    }}

    #[test]
    fn reset_sets_the_count_to_zero() {{
        assert_eq!(run(true, Some(7), Message::Reset), counted(0));
    }}

    #[test]
    #[should_panic(expected = "must authorize")]
    fn an_unauthorized_account_is_refused() {{
        let _refused = run(false, Some(1), Message::Increment {{ by: 1 }});
    }}
}}
"#
            ),
        ),
        // Guest
        (
            "guest/Cargo.toml".to_owned(),
            format!(
                r#"[package]
name = "{snake}-guest"
version = "0.1.0"
edition = "2021"
# The risc0 guest toolchain's rustc: dependencies resolve to versions that support it.
rust-version = "1.88"

# Its own workspace: the guest builds for the zkVM with the risc0 toolchain.
[workspace]
resolver = "3"

[profile.release]
debug = 0
strip = "symbols"

[[bin]]
name = "{snake}"
path = "src/bin/{snake}.rs"

[dependencies]
lee_core = {{ {lez} }}
{program} = {{ path = "../{program}" }}
# 3.1: every 3.0 release is yanked.
risc0-zkvm = {{ version = "3.1", default-features = false }}
"#
            ),
        ),
        (
            format!("guest/src/bin/{snake}.rs"),
            format!(
                r"//! The {project} guest: decodes the message and runs the handler, one actor per run.

lee_core::actor_entrypoint!({program}::handle_message);
"
            ),
        ),
    ];
    Ok(files)
}

/// Run `spel init`: write the scaffold into a new directory.
///
/// # Errors
/// If the directory exists, the name isn't usable, a dependency can't be pinned, or a file can't
/// be written.
pub fn init_command(args: &InitArgs) -> Result<PathBuf, String> {
    let root = PathBuf::from(&args.project);
    if root.exists() {
        return Err(format!("'{}' already exists", root.display()));
    }
    let (project, snake) = names(&root)?;
    let files = scaffold(&project, &snake, args)?;
    for (rel, content) in files {
        let path = root.join(&rel);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        }
        fs::write(&path, content).map_err(|e| format!("writing {}: {e}", path.display()))?;
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn a_project_name_becomes_the_rust_name() {
        assert_eq!(
            names(Path::new("/tmp/my-counter")).unwrap(),
            ("my-counter".to_owned(), "my_counter".to_owned())
        );
        assert!(names(Path::new("2fast")).is_err());
        assert!(names(Path::new("My-Counter")).is_err());
    }

    #[test]
    fn one_way_to_name_each_dependency() {
        assert!(parse_args(&args("x --lez-tag a --lez-rev b")).is_err());
        assert!(parse_args(&args("x --spel-rev a --spel-path b")).is_err());
        assert!(parse_args(&args("x --spel-path a --spel-git b")).is_err());
        assert!(parse_args(&args("--lez-tag a")).is_err());
        assert_eq!(
            parse_args(&args("x --lez-rev abc --spel-tag v1")).unwrap(),
            InitArgs {
                project: "x".into(),
                lez_rev: Some("abc".into()),
                spel_tag: Some("v1".into()),
                ..InitArgs::default()
            }
        );
    }

    #[test]
    fn dependencies_follow_the_flags() {
        let parsed = parse_args(&args(
            "x --lez-rev abc --spel-tag v1 --spel-git https://g/s",
        ))
        .unwrap();
        assert_eq!(
            lez_dependency(&parsed),
            format!(r#"git = "{LEZ_GIT}", rev = "abc""#)
        );
        assert_eq!(
            spel_attrs_dependency(&parsed).unwrap(),
            r#"git = "https://g/s", tag = "v1""#
        );
        let defaults = parse_args(&args("x")).unwrap();
        assert_eq!(
            lez_dependency(&defaults),
            format!(r#"git = "{LEZ_GIT}", tag = "{DEFAULT_LEZ_TAG}""#)
        );
        if !SPEL_GIT_REV.is_empty() {
            assert_eq!(
                spel_attrs_dependency(&defaults).unwrap(),
                format!(r#"git = "{SPEL_GIT}", rev = "{SPEL_GIT_REV}""#)
            );
        }
    }

    #[test]
    fn a_spel_path_must_hold_spel_attrs() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let parsed = parse_args(&args(&format!("x --spel-path {}", repo.display()))).unwrap();
        let dependency = spel_attrs_dependency(&parsed).unwrap();
        assert!(dependency.starts_with("path = \"") && dependency.ends_with("spel-attrs\""));
        let empty = tempfile::tempdir().unwrap();
        let parsed =
            parse_args(&args(&format!("x --spel-path {}", empty.path().display()))).unwrap();
        assert!(spel_attrs_dependency(&parsed)
            .unwrap_err()
            .contains("no spel-attrs"));
    }

    #[test]
    fn the_scaffold_names_everything_after_the_project() {
        let parsed = parse_args(&args("my-counter --spel-tag v1")).unwrap();
        let files = scaffold("my-counter", "my_counter", &parsed).unwrap();
        let file = |name: &str| {
            &files
                .iter()
                .find(|(path, _)| path == name)
                .unwrap_or_else(|| panic!("no {name}"))
                .1
        };
        assert!(file("spel.toml").contains("[programs.my_counter]"));
        assert!(file("spel.toml").contains("core   = \"my_counter_core\""));
        assert!(file("my_counter_core/src/lib.rs")
            .contains(r#"#[cfg_attr(feature = "idl", spel_attrs::message)]"#));
        assert!(file("guest/src/bin/my_counter.rs")
            .contains("actor_entrypoint!(my_counter_program::handle_message)"));
        assert!(
            file("Cargo.toml").contains(r#"members = ["my_counter_core", "my_counter_program"]"#)
        );
    }
}
