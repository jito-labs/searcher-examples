mod orca;
mod orca_utils;

use std::{env, rc::Rc, sync::Arc};

use anchor_client::{Client as AnchorClient, Cluster};
use clap::Parser;
use env_logger::TimestampPrecision;
use jito_protos::searcher::SubscribeBundleResultsRequest;
use jito_searcher_client::{get_searcher_client, send_bundle_with_confirmation};
use orca::swap;
use solana_client::{nonblocking::rpc_client::RpcClient as AsyncRpcClient, rpc_client::RpcClient};
use solana_client_helpers::Client as SolanaClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    signature::{read_keypair_file, Signer},
    transaction::{Transaction, VersionedTransaction},
};
use spl_memo::build_memo;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about=None)]
struct Args {
    #[clap(long, env)]
    rpc_url: String,

    #[clap(long, env)]
    rpc_ws_url: String,

    #[clap(long, env)]
    keypair_path: String,

    #[clap(long, env)]
    block_engine_url: String,

    #[clap(long, env)]
    tip_account: String,
}

/// Example of how to build orca swap from SAMO-> USDC with a memo and submit as a bundle
fn main() {
    let args: Args = Args::parse();
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info")
    }
    env_logger::builder()
        .format_timestamp(Some(TimestampPrecision::Micros))
        .init();

    let connection =
        RpcClient::new_with_commitment(args.rpc_url.to_string(), CommitmentConfig::confirmed());

    let payer =
        read_keypair_file(args.keypair_path.clone()).expect("example requires a keypair file");

    let payer_pubkey = payer.pubkey();

    let solana_client = SolanaClient {
        client: connection,
        payer,
    };
    let auth_keypair =
        Arc::new(read_keypair_file(args.keypair_path.clone()).expect("reads keypair at path"));

    // build Anchor client
    let cluster = Cluster::Custom(args.rpc_url.clone(), args.rpc_ws_url);
    let payer =
        read_keypair_file(args.keypair_path.clone()).expect("example requires a keypair file");
    let anchor_client = AnchorClient::new_with_options(
        cluster.clone(),
        Rc::new(payer),
        CommitmentConfig::confirmed(),
    );

    let program = anchor_client.program(whirlpool::id());
    let instructions = swap(100000, &solana_client, program, payer_pubkey).unwrap();

    let blockhash = solana_client
        .client
        .get_latest_blockhash()
        .expect("get blockhash");

    let payer =
        read_keypair_file(args.keypair_path.clone()).expect("example requires a keypair file");
    // TODO: make these mutable so that we can pass in a more recent blockhash once jito is leader
    let tx_0 = VersionedTransaction::from(Transaction::new_signed_with_payer(
        &[build_memo(
            format!("i b swimmin in da mempool 🏊🏊🏊🏊").as_bytes(),
            &[],
        )],
        Some(&payer_pubkey),
        &[&payer],
        blockhash.clone(),
    ));
    let tx_1 = VersionedTransaction::from(Transaction::new_signed_with_payer(
        instructions.as_ref(),
        Some(&payer_pubkey),
        &[&payer],
        blockhash.clone(),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let mut searcher_client =
            get_searcher_client(args.block_engine_url.as_str(), &auth_keypair)
                .await
                .unwrap();

        let mut bundle_results_subscription = searcher_client
            .subscribe_bundle_results(SubscribeBundleResultsRequest {})
            .await
            .expect("subscribe to bundle results")
            .into_inner();

        send_bundle_with_confirmation(
            &[tx_0, tx_1],
            &AsyncRpcClient::new(args.rpc_url.clone()),
            &mut searcher_client,
            &mut bundle_results_subscription,
        )
        .await
        .unwrap();
    });
}
