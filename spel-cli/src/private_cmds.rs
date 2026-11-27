//! Commands for private accounts: `spel pda` with `--owner` / `--group`, `spel blind`, and
//! `spel transfer`.

use std::process;

use lee::AccountId;
use lee_core::program::PdaSeed;
use sha2::{Digest as _, Sha256};
use wallet::account::AccountIdWithPrivacy;
use wallet::program_facades::{native_token_transfer::NativeTokenTransfer, CreditDelivery};
use wallet::AccountIdentity;

use crate::deploy::StdoutToStderr;
use crate::hex::{decode_bytes_32, hex_decode, hex_encode};
use crate::private::{self, Keys};
use crate::send::{open_wallet, Names, Wallet};

pub const PDA_USAGE: &str = "Usage: spel --program <PROGRAM> pda <SEED>... (--owner <ACCOUNT> | \
     --group <GROUP>) [--register [--label <LABEL>] [--rescan]]";

pub const BLIND_USAGE: &str = "Usage: spel blind <BASE> <FACTOR> [--hex]";

pub const TRANSFER_USAGE: &str = "Usage: spel [--program <P>] transfer --from <ACCOUNT> \
     --to <ACCOUNT | keys:FILE | pda:SEED[,SEED...]> --amount <N>";

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("❌ {msg}");
    process::exit(1);
}

fn open_wallet_or_fail() -> Wallet {
    open_wallet(None).unwrap_or_else(|e| {
        fail(format!(
            "Failed to open the wallet: {e}\n   Set LEE_WALLET_HOME_DIR"
        ))
    })
}

/// Whether `args` ask `spel pda` for a private PDA.
#[must_use]
pub fn is_private_pda(args: &[String]) -> bool {
    args.iter().any(|a| a == "--owner" || a == "--group")
}

/// A seed argument as 32 bytes: `account:<label | id>` (the account's id), 64 hex digits with or
/// without `0x`, or a string of at most 32 bytes, zero-padded.
///
/// # Errors
/// If an account doesn't resolve, or a string is too long.
pub fn seed_bytes(raw: &str, names: &Names<'_>) -> Result<[u8; 32], String> {
    if let Some(account) = raw.strip_prefix("account:") {
        return Ok(*names.account(account)?.value());
    }
    if let Some(bytes) = hex32(raw) {
        return Ok(bytes);
    }
    if raw.len() > 32 {
        return Err(format!("seed '{raw}' is {} bytes, max 32", raw.len()));
    }
    let mut bytes = [0_u8; 32];
    for (byte, src) in bytes.iter_mut().zip(raw.as_bytes()) {
        *byte = *src;
    }
    Ok(bytes)
}

/// 64 hex digits, with or without `0x`.
fn hex32(raw: &str) -> Option<[u8; 32]> {
    let digits = raw.strip_prefix("0x").unwrap_or(raw);
    if digits.len() != 64 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    hex_decode(digits).ok()?.try_into().ok()
}

/// Seeds combine as `spel pda`'s public PDAs do: one is itself, several are their SHA-256.
#[must_use]
pub fn combine_seeds(seeds: &[[u8; 32]]) -> PdaSeed {
    if let [seed] = seeds {
        return PdaSeed::new(*seed);
    }
    let mut hasher = Sha256::new();
    for seed in seeds {
        hasher.update(seed);
    }
    PdaSeed::new(hasher.finalize().into())
}

/// `spel pda … --owner | --group`: derive a private PDA, and with `--register` record it in the
/// wallet so `send` can witness it.
pub async fn pda_command(args: &[String], program: &str, programs: Vec<(String, AccountId)>) {
    let mut seeds_raw = Vec::new();
    let (mut owner, mut group, mut label) = (None, None, None);
    let (mut register, mut rescan) = (false, false);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| {
            iter.next()
                .cloned()
                .unwrap_or_else(|| fail(format!("{flag} needs a value\n{PDA_USAGE}")))
        };
        match arg.as_str() {
            "--owner" => owner = Some(value("--owner")),
            "--group" => group = Some(value("--group")),
            "--label" => label = Some(value("--label")),
            "--register" => register = true,
            "--rescan" => rescan = true,
            flag if flag.starts_with("--") => {
                fail(format!("unknown pda option '{flag}'\n{PDA_USAGE}"))
            },
            seed => seeds_raw.push(seed.to_owned()),
        }
    }
    let keys = match (&owner, &group) {
        (Some(owner), None) => Keys::Owner(owner),
        (None, Some(group)) => Keys::Group(group),
        _ => fail(format!(
            "name exactly one of --owner and --group\n{PDA_USAGE}"
        )),
    };
    if seeds_raw.is_empty() {
        fail(format!("at least one seed\n{PDA_USAGE}"));
    }
    if (label.is_some() || rescan) && !register {
        fail("--label and --rescan go with --register");
    }

    let mut wallet = open_wallet_or_fail();
    let program_id = Names {
        wallet: Some(&wallet),
        programs: programs.clone(),
    }
    .program(program)
    .unwrap_or_else(|e| fail(e));
    // A group's PDA keys derive from the program's ImageID: the deployed code's, read from its
    // header, so a stale local build can't derive another address.
    let image = match keys {
        Keys::Group(_) => Some(
            private::program_header(&wallet, program_id)
                .await
                .unwrap_or_else(|e| fail(e))
                .image_id,
        ),
        Keys::Owner(_) => None,
    };
    let (program, pda) = {
        let names = Names {
            wallet: Some(&wallet),
            programs,
        };
        let program = names.program(program).unwrap_or_else(|e| fail(e));
        let seeds = seeds_raw
            .iter()
            .map(|raw| seed_bytes(raw, &names))
            .collect::<Result<Vec<_>, String>>()
            .unwrap_or_else(|e| fail(e));
        let pda = private::derive_private_pda(&names, program, combine_seeds(&seeds), &keys, image)
            .unwrap_or_else(|e| fail(e));
        (program, pda)
    };
    let account = pda.account;
    if register {
        let new = private::register(&mut wallet, program, pda, label.as_deref())
            .unwrap_or_else(|e| fail(e));
        eprintln!(
            "{} Private/{account}",
            if new {
                "registered"
            } else {
                "already registered:"
            }
        );
        if rescan {
            eprintln!("rescanning private accounts from genesis...");
            private::sync(&mut wallet, true)
                .await
                .unwrap_or_else(|e| fail(e));
        }
    }
    println!("{account}");
}

/// A 32-byte value for `spel blind`: `le64:N` (N as 8 little-endian bytes, zero-padded), 64 hex
/// digits with or without `0x`, an account id, or a wallet label.
fn blind_value(raw: &str, wallet: &mut Option<Wallet>) -> [u8; 32] {
    if let Some(n) = raw.strip_prefix("le64:") {
        let n: u64 = n.parse().unwrap_or_else(|e| fail(format!("{raw}: {e}")));
        let mut bytes = [0_u8; 32];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        return bytes;
    }
    if let Some(bytes) = hex32(raw) {
        return bytes;
    }
    if let Ok(bytes) = decode_bytes_32(raw) {
        return bytes;
    }
    let wallet = wallet.get_or_insert_with(open_wallet_or_fail);
    let names = Names {
        wallet: Some(wallet),
        programs: Vec::new(),
    };
    *names.account(raw).unwrap_or_else(|e| fail(e)).value()
}

/// `spel blind <BASE> <FACTOR>`: `AccountId::blinded`, as base58 (or `0x` hex with `--hex`).
pub fn blind_command(args: &[String]) {
    let hex = args.iter().any(|a| a == "--hex");
    let values: Vec<&String> = args.iter().filter(|a| *a != "--hex").collect();
    let [base, factor] = values.as_slice() else {
        fail(BLIND_USAGE)
    };
    let mut wallet = None;
    let base = AccountId::new(blind_value(base, &mut wallet));
    let factor = blind_value(factor, &mut wallet);
    let blinded = base.blinded(&factor);
    if hex {
        println!("0x{}", hex_encode(blinded.value()));
    } else {
        println!("{blinded}");
    }
}

/// The destination `pda:SEED[,SEED...]`: `program`'s public PDA, named by its program and seed so
/// the transaction carries the evidence that admits it. A fresh public account can't be credited
/// by its bare id: nothing shows that someone controls it.
///
/// # Errors
/// If a seed is malformed.
pub fn public_pda_destination(
    seeds: &str,
    program: AccountId,
    names: &Names<'_>,
) -> Result<AccountIdentity, String> {
    let seeds = seeds
        .split(',')
        .map(|raw| seed_bytes(raw, names))
        .collect::<Result<Vec<_>, String>>()
        .map_err(|e| format!("--to pda:{seeds}: {e}"))?;
    Ok(AccountIdentity::PublicPda {
        program,
        seed: combine_seeds(&seeds),
    })
}

/// `spel transfer`: move native balance between any two accounts this wallet can act for, to a
/// private account known by its keys (`keys:FILE`, whose credit waits for its owner), or to a
/// public PDA of `--program` (`pda:SEED`, which a first deposit creates).
pub async fn transfer_command(
    args: &[String],
    program: Option<&str>,
    programs: Vec<(String, AccountId)>,
) {
    let (mut from, mut to, mut amount) = (None, None, None);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| {
            iter.next()
                .cloned()
                .unwrap_or_else(|| fail(format!("{flag} needs a value\n{TRANSFER_USAGE}")))
        };
        match arg.as_str() {
            "--from" => from = Some(value("--from")),
            "--to" => to = Some(value("--to")),
            "--amount" => {
                let raw = value("--amount");
                amount = Some(
                    raw.parse::<u128>()
                        .unwrap_or_else(|e| fail(format!("--amount {raw}: {e}"))),
                );
            },
            other => fail(format!(
                "unknown transfer argument '{other}'\n{TRANSFER_USAGE}"
            )),
        }
    }
    let (Some(from), Some(to), Some(amount)) = (from, to, amount) else {
        fail(TRANSFER_USAGE)
    };

    let mut wallet = open_wallet_or_fail();
    let (sender, recipient) = {
        let names = Names {
            wallet: Some(&wallet),
            programs,
        };
        let sender = match names
            .account_with_privacy(&from)
            .unwrap_or_else(|e| fail(e))
        {
            AccountIdWithPrivacy::Public(id) => AccountIdentity::Public(id),
            AccountIdWithPrivacy::Private(id) => {
                private::private_identity(&wallet, id).unwrap_or_else(|e| fail(e))
            },
        };
        let recipient = if let Some(seeds) = to.strip_prefix("pda:") {
            let program = program.unwrap_or_else(|| {
                fail(format!(
                    "--to {to}: a PDA is its program's; pass --program\n{TRANSFER_USAGE}"
                ))
            });
            let program = names.program(program).unwrap_or_else(|e| fail(e));
            public_pda_destination(seeds, program, &names).unwrap_or_else(|e| fail(e))
        } else if to.starts_with("keys:") {
            private::destination(&names, &to)
                .unwrap_or_else(|e| fail(e))
                .identity
        } else {
            match names.account_with_privacy(&to).unwrap_or_else(|e| fail(e)) {
                AccountIdWithPrivacy::Public(id) => AccountIdentity::PublicNoSign(id),
                AccountIdWithPrivacy::Private(id) => {
                    private::private_identity(&wallet, id).unwrap_or_else(|e| fail(e))
                },
            }
        };
        (sender, recipient)
    };
    let hash = {
        let _quiet = StdoutToStderr::new();
        NativeTokenTransfer(&wallet)
            .transfer(sender, recipient, amount, CreditDelivery::Automatic)
            .await
            .unwrap_or_else(|e| fail(format!("transfer refused by the wallet: {e:#}")))
            .0
    };
    eprintln!("Submitted {hash}, waiting for a block...");
    let (block, reverted) = private::settle(&mut wallet, hash)
        .await
        .unwrap_or_else(|e| fail(e));
    println!("{hash}");
    if reverted {
        eprintln!("⚠️  Included in block {block}, but it reverted: its fee was charged.");
        process::exit(crate::send::EXIT_NO_CHANGE);
    }
    eprintln!("Included in block {block}: moved {amount} from {from} to {to}.");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names<'static> {
        Names {
            wallet: None,
            programs: Vec::new(),
        }
    }

    #[test]
    fn seeds_take_hex_accounts_and_strings() {
        let id = AccountId::new([5; 32]);
        assert_eq!(
            seed_bytes(&format!("account:{id}"), &names()).unwrap(),
            [5; 32]
        );
        assert_eq!(seed_bytes(&"ab".repeat(32), &names()).unwrap(), [0xab; 32]);
        assert_eq!(
            seed_bytes(&format!("0x{}", "cd".repeat(32)), &names()).unwrap(),
            [0xcd; 32]
        );
        let mut padded = [0_u8; 32];
        padded[..5].copy_from_slice(b"vault");
        assert_eq!(seed_bytes("vault", &names()).unwrap(), padded);
        assert!(seed_bytes(&"x".repeat(33), &names()).is_err());
    }

    #[test]
    fn one_seed_is_itself_and_several_are_hashed() {
        assert_eq!(combine_seeds(&[[1; 32]]), PdaSeed::new([1; 32]));
        let mut hasher = Sha256::new();
        hasher.update([1; 32]);
        hasher.update([2; 32]);
        let hashed: [u8; 32] = hasher.finalize().into();
        assert_eq!(combine_seeds(&[[1; 32], [2; 32]]), PdaSeed::new(hashed));
    }

    /// The multisig example's hashes, as `spel blind` composes them.
    #[test]
    fn blind_composes_seeds_and_pseudonyms() {
        let mut none = None;
        let create_key = [7_u8; 32];
        let generation = blind_value("le64:3", &mut none);
        let mut word = [0_u8; 32];
        word[..8].copy_from_slice(&3_u64.to_le_bytes());
        assert_eq!(generation, word);
        // seed_for(create_key, 3) = AccountId::new(create_key).blinded(le64(3))
        let seed = AccountId::new(create_key).blinded(&generation);
        assert_ne!(seed, AccountId::new(create_key));
        // pseudonym(seat, secret, create_key) = seat.blinded(AccountId::new(secret).blinded(ck))
        let (seat, secret) = (AccountId::new([1; 32]), [2_u8; 32]);
        let factor = AccountId::new(secret).blinded(&create_key);
        let via_text = blind_value(&factor.to_string(), &mut none);
        assert_eq!(seat.blinded(&via_text), seat.blinded(factor.value()));
    }
}
