use alloy_primitives::Address;
use alloy_sol_types::{sol, SolCall};
use anyhow::Result;
use apps::TxSender;
use aragon_zk_voting_protocol_methods::VOTING_PROTOCOL_ELF;
use clap::Parser;
use risc0_ethereum_contracts::groth16::encode;
use risc0_steel::{config::ETH_SEPOLIA_CHAIN_SPEC, ethereum::EthEvmEnv, Contract, EvmBlockHeader};
use risc0_zkvm::{default_prover, ExecutorEnv, ProverOpts, VerifierContext};
use tracing_subscriber::EnvFilter;

sol! {
    /// ERC-20 balance function signature.
    /// This must match the signature in the guest.
    interface IERC20 {
        function balanceOf(address account) external view returns (uint);
    }
    interface ConfigContract {
        function getConfig() external view returns (string memory);
    }
}

sol!("../contracts/ICounter.sol");

/// Arguments of the publisher CLI.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Ethereum chain ID
    #[clap(long)]
    chain_id: u64,

    /// Ethereum Node endpoint.
    #[clap(long, env)]
    eth_wallet_private_key: String,

    /// Ethereum Node endpoint.
    #[clap(long, env)]
    rpc_url: String,

    /// Ethereum block number.
    #[clap(long)]
    block_number: Option<u64>,

    /// Counter's contract address on Ethereum
    #[clap(long)]
    contract: Address,

    /// ERC20 contract address on Ethereum
    #[clap(long)]
    token: Address,

    /// Account address to read the balance_of on Ethereum
    #[clap(long)]
    account: Address,
}

fn main() -> Result<()> {
    // Initialize tracing. In order to view logs, run `RUST_LOG=info cargo run`
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // parse the command line arguments
    let args = Args::parse();

    // Create an EVM environment from an RPC endpoint and a block number. If no block number is
    // provided, the latest block is used.
    let mut env = EthEvmEnv::from_rpc(&args.rpc_url, args.block_number)?;
    //  The `with_chain_spec` method is used to specify the chain configuration.
    env = env.with_chain_spec(&ETH_SEPOLIA_CHAIN_SPEC);

    // Making the preflighs. This step is mandatory
    let primary_call = ConfigContract::getConfigCall {};
    let mut primary_contract = Contract::preflight(args.contract, &mut env);
    let primary_returns = primary_contract.call_builder(&primary_call).call()?;
    println!("Primary contract returns: {:?}", primary_returns._0);

    // Prepare the function call
    let call = IERC20::balanceOfCall {
        account: args.account,
    };

    // Preflight the call to execute the function in the guest.
    let mut contract = Contract::preflight(args.token, &mut env);
    let returns = contract.call_builder(&call).call()?;
    println!(
        "For block {} calling `{}` on {} returns: {}",
        env.header().number(),
        IERC20::balanceOfCall::SIGNATURE,
        args.token,
        returns._0
    );

    println!("proving...");
    let view_call_input = env.into_input()?;
    let env = ExecutorEnv::builder()
        .write(&view_call_input)?
        .write(&args.account)?
        .write(&args.contract)?
        .build()?;

    let receipt = default_prover()
        .prove_with_ctx(
            env,
            &VerifierContext::default(),
            VOTING_PROTOCOL_ELF,
            &ProverOpts::groth16(),
        )?
        .receipt;
    println!("proving...done");

    // Create a new `TxSender`.
    let tx_sender = TxSender::new(
        args.chain_id,
        &args.rpc_url,
        &args.eth_wallet_private_key,
        &args.contract.to_string(),
    )?;

    // Encode the groth16 seal with the selector
    let seal = encode(receipt.inner.groth16()?.seal.clone())?;

    // Encode the function call for `ICounter.increment(journal, seal)`.
    let calldata = ICounter::incrementCall {
        journalData: receipt.journal.bytes.into(),
        seal: seal.into(),
    }
    .abi_encode();

    // Send the calldata to Ethereum.
    println!("sending tx...");
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(tx_sender.send(calldata))?;
    println!("sending tx...done");

    Ok(())
}
