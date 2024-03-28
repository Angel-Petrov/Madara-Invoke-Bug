use std::{
    fs,
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime},
};

use color_eyre::eyre::{bail, eyre};
use log::{debug, info, warn};
use starknet::{
    accounts::{Account, Call, ConnectedAccount, ExecutionEncoding, SingleOwnerAccount},
    contract::ContractFactory,
    core::{
        crypto::compute_hash_on_elements,
        types::{
            contract::legacy::LegacyContractClass, BlockId, BlockTag, ExecutionResult,
            FieldElement, MaybePendingTransactionReceipt, StarknetError,
        },
    },
    macros::{felt, selector},
    providers::{
        jsonrpc::HttpTransport, JsonRpcClient, MaybeUnknownErrorCode, Provider, ProviderError,
        StarknetErrorWithMessage,
    },
    signers::{LocalWallet, SigningKey},
};
use url::Url;

pub static CHECK_INTERVAL: Duration = Duration::from_millis(500);

const MAX_FEE: FieldElement = felt!("0x6efb28c75a0000");

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    // Initialize the logger.
    env_logger::init();

    // Initialize the error handler.
    color_eyre::install()?;

    let args: usize = std::env::args()
        .nth(1)
        .as_deref()
        .map(FromStr::from_str)
        .transpose()?
        .unwrap_or(1000);

    let starknet_rpc = Arc::new(JsonRpcClient::new(HttpTransport::new(Url::parse(
        "http://localhost:9944",
    )?)));

    let signer = LocalWallet::from(SigningKey::from_secret_scalar(felt!(
        "0x00c1cf1490de1352865301bb8705143f3ef938f97fdf892f1090dcb5ac7bcd1d"
    )));

    let mut account = SingleOwnerAccount::new(
        starknet_rpc.clone(),
        signer.clone(),
        felt!("0x0000000000000000000000000000000000000000000000000000000000000004"),
        FieldElement::from_byte_slice_be(b"SN_GOERLI")?,
        ExecutionEncoding::New,
    );

    let erc20_contract_artifact: LegacyContractClass =
        serde_json::from_str(&fs::read_to_string("ERC20.json")?)?;

    let class_hash = erc20_contract_artifact.class_hash()?;

    let mut nonce = account.get_nonce().await?;

    let class_hash = if check_already_declared(&starknet_rpc, class_hash).await? {
        info!("Contract is already declared");

        class_hash
    } else {
        account.set_block_id(BlockId::Tag(BlockTag::Pending));

        let tx_resp = account
            .declare_legacy(Arc::new(erc20_contract_artifact))
            .max_fee(MAX_FEE)
            .nonce(nonce)
            .send()
            .await?;

        wait_for_tx(&starknet_rpc, tx_resp.transaction_hash, CHECK_INTERVAL).await?;

        nonce += FieldElement::ONE;

        tx_resp.class_hash
    };

    let contract_factory = ContractFactory::new(class_hash, &account);

    let name = selector!("TestToken");
    let symbol = selector!("TT");
    let decimals = felt!("128");
    let (initial_supply_low, initial_supply_high) = (felt!("0xFFFFFFFFF"), felt!("0xFFFFFFFFF"));
    let recipient = account.address();

    let constructor_args = vec![
        name,
        symbol,
        decimals,
        initial_supply_low,
        initial_supply_high,
        recipient,
    ];
    let unique = false;

    let address = compute_contract_address(felt!("1"), class_hash, &constructor_args);

    if let Ok(contract_class_hash) = starknet_rpc
        .get_class_hash_at(BlockId::Tag(BlockTag::Pending), address)
        .await
    {
        if contract_class_hash == class_hash {
            warn!("ERC20 contract already deployed at address {address:#064x}");
        } else {
            bail!("ERC20 contract {address:#064x} already deployed with a different class hash {contract_class_hash:#064x}, expected {class_hash:#064x}");
        }
    } else {
        let deploy = contract_factory.deploy(constructor_args, felt!("1"), unique);

        info!(
            "Deploying ERC20 contract with nonce={}, address={:#064x}",
            nonce, address
        );

        let result = deploy.nonce(nonce).max_fee(MAX_FEE).send().await?;
        wait_for_tx(&starknet_rpc, result.transaction_hash, CHECK_INTERVAL).await?;

        nonce += FieldElement::ONE;

        debug!(
            "Deploy ERC20 transaction accepted {:#064x}",
            result.transaction_hash
        );
    }

    info!("ERC20 contract deployed at address {:#064x}", address);

    let (amount_low, amount_high) = (felt!("1"), felt!("0"));

    // Hex: 0xdead
    // from_hex_be isn't const whereas from_mont is
    const VOID_ADDRESS: FieldElement = FieldElement::from_mont([
        18446744073707727457,
        18446744073709551615,
        18446744073709551615,
        576460752272412784,
    ]);

    let call = Call {
        to: address,
        selector: selector!("transfer"),
        calldata: vec![VOID_ADDRESS, amount_low, amount_high],
    };

    let mut vec = Vec::with_capacity(1000);

    for _ in 0..args {
        let result = account
            .execute(vec![call.clone()])
            .max_fee(MAX_FEE)
            .nonce(nonce)
            .send()
            .await?;

        vec.push(result.transaction_hash);

        nonce += FieldElement::ONE;
    }

    for hash in vec {
        wait_for_tx(&starknet_rpc, hash, CHECK_INTERVAL).await?;
    }

    Ok(())
}

/// Cairo string for "STARKNET_CONTRACT_ADDRESS"
const PREFIX_CONTRACT_ADDRESS: FieldElement = FieldElement::from_mont([
    3829237882463328880,
    17289941567720117366,
    8635008616843941496,
    533439743893157637,
]);

/// 2 ** 251 - 256
const ADDR_BOUND: FieldElement = FieldElement::from_mont([
    18446743986131443745,
    160989183,
    18446744073709255680,
    576459263475590224,
]);

pub fn compute_contract_address(
    salt: FieldElement,
    class_hash: FieldElement,
    constructor_calldata: &[FieldElement],
) -> FieldElement {
    compute_hash_on_elements(&[
        PREFIX_CONTRACT_ADDRESS,
        FieldElement::ZERO,
        salt,
        class_hash,
        compute_hash_on_elements(constructor_calldata),
    ]) % ADDR_BOUND
}

async fn check_already_declared(
    starknet_rpc: &JsonRpcClient<HttpTransport>,
    class_hash: FieldElement,
) -> color_eyre::Result<bool> {
    match starknet_rpc
        .get_class(BlockId::Tag(BlockTag::Pending), class_hash)
        .await
    {
        Ok(_) => {
            warn!("Contract already declared at {class_hash:#064x}");
            Ok(true)
        }
        Err(ProviderError::StarknetError(StarknetErrorWithMessage {
            code: MaybeUnknownErrorCode::Known(StarknetError::ClassHashNotFound),
            ..
        })) => Ok(false),
        Err(err) => Err(eyre!(err)),
    }
}

const WAIT_FOR_TX_TIMEOUT: Duration = Duration::from_secs(60);

pub async fn wait_for_tx(
    provider: &JsonRpcClient<HttpTransport>,
    tx_hash: FieldElement,
    check_interval: Duration,
) -> color_eyre::Result<()> {
    let start = SystemTime::now();

    loop {
        if start.elapsed().unwrap() >= WAIT_FOR_TX_TIMEOUT {
            bail!("Timeout while waiting for transaction {tx_hash:#064x}");
        }

        match provider.get_transaction_receipt(tx_hash).await {
            Ok(MaybePendingTransactionReceipt::Receipt(receipt)) => {
                match receipt.execution_result() {
                    ExecutionResult::Succeeded => {
                        return Ok(());
                    }
                    ExecutionResult::Reverted { reason } => {
                        bail!(format!(
                            "Transaction {tx_hash:#064x} has been rejected/reverted: {reason}"
                        ));
                    }
                }
            }
            Ok(MaybePendingTransactionReceipt::PendingReceipt(pending)) => {
                if let ExecutionResult::Reverted { reason } = pending.execution_result() {
                    bail!(format!(
                        "Transaction {tx_hash:#064x} has been rejected/reverted: {reason}"
                    ));
                }
                debug!("Waiting for transaction {tx_hash:#064x} to be accepted");
                tokio::time::sleep(check_interval).await;
            }
            Err(ProviderError::StarknetError(StarknetErrorWithMessage {
                code: MaybeUnknownErrorCode::Known(StarknetError::TransactionHashNotFound),
                ..
            })) => {
                debug!("Waiting for transaction {tx_hash:#064x} to show up");
                tokio::time::sleep(check_interval).await;
            }
            Err(err) => {
                return Err(eyre!(err).wrap_err(format!(
                    "Error while waiting for transaction {tx_hash:#064x}"
                )))
            }
        }
    }
}
