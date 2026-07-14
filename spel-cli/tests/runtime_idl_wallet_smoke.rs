use std::{env, error::Error, fs, io, time::Duration};

use common::HashType;
use nssa::{
    privacy_preserving_transaction::circuit::ProgramWithDependencies, program::Program, AccountId,
};
use sequencer_service_rpc::RpcClient as _;
use spel::tx::SpelProgram;
use spel_framework_core::{idl::SpelIdl, pda::parse_bytes32};
use tokio::time::sleep;
use wallet::{AccountIdentity, WalletCore};

const IDL_ENV: &str = "SPEL_RUNTIME_IDL_SMOKE_IDL";
const GUEST_BIN_ENV: &str = "SPEL_RUNTIME_IDL_SMOKE_GUEST_BIN";
const PUBLIC_ACCOUNT_ENV: &str = "SPEL_RUNTIME_IDL_SMOKE_PUBLIC_ACCOUNT";
const PRIVATE_ACCOUNT_ENV: &str = "SPEL_RUNTIME_IDL_SMOKE_PRIVATE_ACCOUNT";
const BLOCK_INTERVAL: Duration = Duration::from_secs(20);

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn required_env(name: &str) -> TestResult<String> {
    Ok(env::var(name)?)
}

fn account_id(name: &str) -> TestResult<AccountId> {
    let value = required_env(name)?;
    Ok(AccountId::new(
        parse_bytes32(&value).map_err(io::Error::other)?,
    ))
}

#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions produce clearer failures for this integration test"
)]
#[tokio::test]
#[ignore = "requires scripts/smoke-test-privacy.sh"]
async fn runtime_idl_builds_do_not_submit_or_change_state() -> TestResult {
    // Arrange
    let idl = serde_json::from_slice::<SpelIdl>(&fs::read(required_env(IDL_ENV)?)?)?;
    let program = Program::new(fs::read(required_env(GUEST_BIN_ENV)?)?.into())?;
    let public_program_id = program.id();
    let private_program = ProgramWithDependencies::from(program);
    let public_account = account_id(PUBLIC_ACCOUNT_ENV)?;
    let private_account = account_id(PRIVATE_ACCOUNT_ENV)?;
    let wallet = WalletCore::from_env()?;
    let public_state_before = wallet.sequencer_client.get_account(public_account).await?;
    let private_commitment_before = wallet
        .get_private_account_commitment(private_account)
        .ok_or_else(|| io::Error::other("private account commitment is unavailable"))?;

    // Act
    let public_transaction = SpelProgram::new(&idl)
        .program(public_program_id)
        .public("greet")?
        .input("account", public_account)?
        .input("greeting", "72,101,108,108,111,32,80,117,98,108,105,99")?
        .build(&wallet)
        .await?;
    let (private_transaction, _) = SpelProgram::new(&idl)
        .program(&private_program)
        .private("greet")?
        .input("account", AccountIdentity::PrivateOwned(private_account))?
        .input(
            "greeting",
            "72,101,108,108,111,32,80,114,105,118,97,116,101",
        )?
        .build(&wallet)
        .await?;
    let public_hash = HashType(public_transaction.hash());
    let private_hash = HashType(private_transaction.hash());

    sleep(BLOCK_INTERVAL).await;

    // Assert
    assert!(
        wallet
            .sequencer_client
            .get_transaction(public_hash)
            .await?
            .is_none(),
        "public build submitted a transaction"
    );
    assert!(
        wallet
            .sequencer_client
            .get_transaction(private_hash)
            .await?
            .is_none(),
        "private build submitted a transaction"
    );
    assert_eq!(
        wallet.sequencer_client.get_account(public_account).await?,
        public_state_before,
        "public build changed account state"
    );
    assert_eq!(
        wallet.get_private_account_commitment(private_account),
        Some(private_commitment_before),
        "private build changed Wallet account state"
    );

    Ok(())
}
