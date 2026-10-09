# shellcheck shell=bash
# The funded account the e2e scripts pay fees from. Source it, then:
#
#     FEE_PAYER=$(lez_fee_payer "$WALLET_BIN" "$WALLET_PASSWORD")
#
# Since LEZ v0.2.5 every public transaction pays a fee, and the accounts these
# tests create (program_loader segment and header accounts, fresh signers) hold
# nothing. The debug sequencer's genesis funds a public account whose signing
# key is LEZ's published test key (testnet_initial_state::PRIVATE_KEY_PUB_ACC_A,
# account 6iArKUXx...). A fresh wallet does not hold it, so import it into the
# wallet LEE_WALLET_HOME_DIR points at and print the account id the wallet
# reports. Debug networks only.

LEZ_DEBUG_GENESIS_KEY="10a26a9aec7d34b82364eeae45c5294dbb0a764b000b94eeb9b58511dc487c4d"

lez_fee_payer() {
    local wallet_bin="$1" password="$2"
    # The password on stdin initializes the wallet's storage if this is its
    # first run; an existing storage doesn't read it.
    printf '%s\n' "$password" \
        | "$wallet_bin" account import public --private-key "$LEZ_DEBUG_GENESIS_KEY" 2>&1 \
        | sed -n 's/.*Public\/\([A-Za-z0-9]*\).*/\1/p' | tail -1
}
