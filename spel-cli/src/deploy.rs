//! `spel deploy`: put a program on chain through LEZ's `program_loader`.
//!
//! Since LEZ v0.2.5 there is no `wallet deploy-program`. A program is a chain of
//! segment accounts holding its bytecode plus a header account pointing at the
//! chain, and the program is addressed by that header account from then on. The
//! wallet's `program-loader deploy` does the upload but takes every account as an
//! argument, so a caller first has to create one header and exactly
//! `ceil(len / MAX_SEGMENT_DATA_LEN)` segment accounts by hand. This command does
//! that bookkeeping and prints the header address.

use std::path::Path;
use std::process;

use nssa::AccountId;
use program_loader_core::{MAX_PROGRAM_SEGMENTS, MAX_SEGMENT_DATA_LEN};
use wallet::program_facades::program_loader::ProgramLoader;
use wallet::WalletCore;

use crate::hex::decode_bytes_32;

/// Number of segment accounts `program_loader` needs for a binary of `len` bytes.
pub fn segment_count(len: usize) -> usize {
    len.div_ceil(MAX_SEGMENT_DATA_LEN)
}

/// Deploy `binary` and print its program address on stdout. Everything else,
/// including what the wallet library prints, goes to stderr, so
/// `ADDR=$(spel deploy ...)` works.
///
/// The header and segment accounts are created fresh in the wallet and hold
/// nothing, so they cannot pay their own fees: `fee_payer` names a funded account
/// in the same wallet that pays for every segment and header transaction.
pub async fn deploy_command(binary: &Path, fee_payer: Option<&str>, immutable: bool) {
    let bytes = std::fs::read(binary).unwrap_or_else(|e| {
        eprintln!(
            "❌ Cannot read program binary '{}': {}",
            binary.display(),
            e
        );
        process::exit(1);
    });
    if bytes.is_empty() {
        eprintln!("❌ Program binary '{}' is empty", binary.display());
        process::exit(1);
    }
    let n_segments = segment_count(bytes.len());
    if n_segments > MAX_PROGRAM_SEGMENTS {
        eprintln!(
            "❌ '{}' is {} bytes: {} segments of {} bytes, over program_loader's cap of {}",
            binary.display(),
            bytes.len(),
            n_segments,
            MAX_SEGMENT_DATA_LEN,
            MAX_PROGRAM_SEGMENTS
        );
        process::exit(1);
    }

    let payer = match fee_payer {
        Some(raw) => AccountId::new(decode_bytes_32(raw).unwrap_or_else(|e| {
            eprintln!("❌ --fee-payer '{}': {}", raw, e);
            process::exit(1);
        })),
        None => {
            eprintln!("❌ spel deploy needs --fee-payer <ADDRESS>.");
            eprintln!("   The header and segment accounts are created fresh and hold nothing,");
            eprintln!("   so a funded account in this wallet has to pay for the upload.");
            process::exit(1);
        },
    };

    // The wallet prints progress (whole transactions, storage paths) on stdout.
    // Send it to stderr for the rest of the deploy, so stdout carries nothing
    // but the address printed at the end.
    let quiet = StdoutToStderr::new();

    let mut wallet_core = WalletCore::from_env().await.unwrap_or_else(|e| {
        eprintln!("❌ Failed to initialize wallet: {:?}", e);
        eprintln!("   Set LEE_WALLET_HOME_DIR environment variable");
        process::exit(1);
    });

    // Create the accounts and save them before uploading anything: once a segment
    // lands it is claimed for good, and its key has to survive a failed deploy.
    let (header, _) = wallet_core.create_new_account_public(None);
    let segments: Vec<AccountId> = (0..n_segments)
        .map(|_| wallet_core.create_new_account_public(None).0)
        .collect();
    wallet_core.store_persistent_data().unwrap_or_else(|e| {
        eprintln!("❌ Failed to save the new accounts to the wallet: {:?}", e);
        process::exit(1);
    });

    eprintln!(
        "📦 Deploying {} ({} bytes, {} segment{}), fee payer {}",
        binary.display(),
        bytes.len(),
        n_segments,
        if n_segments == 1 { "" } else { "s" },
        payer
    );
    eprintln!("   header account: {}", header);

    let address = ProgramLoader(&wallet_core)
        .deploy(header, &segments, bytes, immutable, Some(payer))
        .await
        .unwrap_or_else(|e| {
            eprintln!("❌ Deploy failed: {:?}", e);
            process::exit(1);
        });

    drop(quiet);
    eprintln!("✅ Program deployed. Address it with --program {address},");
    eprintln!("   or add `address = \"{address}\"` to its entry in spel.toml.");
    println!("{address}");
}

/// Points file descriptor 1 at stderr until dropped, then restores it.
/// A no-op where that isn't possible (non-Unix, or dup failing).
struct StdoutToStderr {
    #[cfg(unix)]
    saved: Option<i32>,
}

impl StdoutToStderr {
    fn new() -> Self {
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        #[cfg(unix)]
        {
            // SAFETY: dup/dup2/close on the process's own standard descriptors.
            let saved = unsafe {
                let saved = libc::dup(1);
                if saved >= 0 && libc::dup2(2, 1) < 0 {
                    libc::close(saved);
                    -1
                } else {
                    saved
                }
            };
            Self {
                saved: (saved >= 0).then_some(saved),
            }
        }
        #[cfg(not(unix))]
        Self {}
    }
}

impl Drop for StdoutToStderr {
    fn drop(&mut self) {
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        #[cfg(unix)]
        if let Some(saved) = self.saved.take() {
            // SAFETY: restores the descriptor saved in `new`.
            unsafe {
                libc::dup2(saved, 1);
                libc::close(saved);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_count_rounds_up() {
        assert_eq!(segment_count(1), 1);
        assert_eq!(segment_count(MAX_SEGMENT_DATA_LEN), 1);
        assert_eq!(segment_count(MAX_SEGMENT_DATA_LEN + 1), 2);
        assert_eq!(segment_count(3 * MAX_SEGMENT_DATA_LEN), 3);
    }
}
