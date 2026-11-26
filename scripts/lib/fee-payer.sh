# shellcheck shell=bash
# The funded account the e2e scripts pay fees from. Source it, then:
#
#     FEE_PAYER=$(lez_fee_payer "$WALLET_BIN")
#
# The accounts these tests create (program_loader segment and header accounts, fresh signers)
# hold nothing. LEZ's local genesis (lez/sequencer/service/networks/local.json) funds a public
# account whose signing key is LEZ's published test key (testnet_initial_state, account
# CbgR6tj5…). A fresh wallet doesn't hold it, so import it into the wallet LEE_WALLET_HOME_DIR
# points at and print the account id the wallet reports. Debug networks only.
#
# The wallet reads its password from LEE_WALLET_PASSWORD; a fresh wallet is created under it.

LEZ_DEBUG_GENESIS_KEY="7f273098f25b71e6c005a9519f2678da8d1c7f01f6a27778e2d9948abdf901fb"

lez_fee_payer() {
    local wallet_bin="$1"
    "$wallet_bin" account import public --private-key "$LEZ_DEBUG_GENESIS_KEY" 2>&1 \
        | sed -n 's/.*Imported public account Public\/\([A-Za-z0-9]*\).*/\1/p' | tail -1
}
