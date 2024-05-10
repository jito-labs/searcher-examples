use std::{env, path::PathBuf, sync::Arc, time::Duration};

use clap::{Parser, Subcommand};
use env_logger::TimestampPrecision;
use futures_util::StreamExt;
use jito_protos::{
    convert::versioned_tx_from_packet,
    searcher::{
        searcher_service_client::SearcherServiceClient, ConnectedLeadersRegionedRequest,
        GetTipAccountsRequest, NextScheduledLeaderRequest, PendingTxNotification,
        SubscribeBundleResultsRequest,
    },
};
use jito_searcher_client::{
    get_searcher_client_auth, get_searcher_client_no_auth, send_bundle_with_confirmation,
    token_authenticator::ClientInterceptor,
};
use log::info;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{read_keypair_file, Signer},
    system_instruction::transfer,
    transaction::{Transaction, VersionedTransaction},
};
use spl_memo::build_memo;
use tokio::time::{sleep, timeout};
use tonic::{
    codegen::{Body, Bytes, InterceptedService, StdError},
    transport::Channel,
    Streaming,
};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// URL of the block engine.
    /// See: https://jito-labs.gitbook.io/mev/searcher-resources/block-engine#connection-details
    #[arg(long, env)]
    block_engine_url: String,

    /// Path to keypair file used to authenticate with the Jito Block Engine
    /// See: https://jito-labs.gitbook.io/mev/searcher-resources/getting-started#block-engine-api-key
    #[arg(long, env)]
    keypair_path: Option<PathBuf>,

    /// Comma-separated list of regions to request cross-region data from.
    /// If no region specified, then default to the currently connected block engine's region.
    /// Details: https://jito-labs.gitbook.io/mev/searcher-services/recommendations#cross-region
    /// Available regions: https://jito-labs.gitbook.io/mev/searcher-resources/block-engine#connection-details
    #[arg(long, env, value_delimiter = ',')]
    regions: Vec<String>,

    /// Subcommand to run
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Print out information on the next scheduled leader
    NextScheduledLeader,

    /// Prints out information on connected leaders
    ConnectedLeaders,

    /// Prints out connected leaders with their leader slot percentage
    ConnectedLeadersInfo {
        #[clap(long, required = true)]
        rpc_url: String,
    },

    /// Prints out information about the tip accounts
    TipAccounts,

    /// Sends a 1 lamport bundle
    SendBundle {
        /// RPC URL
        #[clap(long, required = true)]
        rpc_url: String,
        /// Filepath to keypair that can afford the transaction payments with 1 lamport tip
        #[clap(long, required = true)]
        payer: PathBuf,
        /// Message you'd like the bundle to say
        #[clap(long, required = true)]
        message: String,
        /// Number of transactions in the bundle (must be <= 5)
        #[clap(long, required = true)]
        num_txs: usize,
        /// Amount of lamports to tip in each transaction
        #[clap(long, required = true)]
        lamports: u64,
        /// One of the tip accounts, see https://jito-foundation.gitbook.io/mev/mev-payment-and-distribution/on-chain-addresses
        #[clap(long, required = true)]
        tip_account: Pubkey,
    },
}

async fn print_next_leader_info(
    client: &mut SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>,
    regions: Vec<String>,
) {
    let next_leader = client
        .get_next_scheduled_leader(NextScheduledLeaderRequest { regions })
        .await
        .expect("gets next scheduled leader")
        .into_inner();
    println!(
        "next jito-solana slot in {} slots for leader {:?}",
        next_leader.next_leader_slot - next_leader.current_slot,
        next_leader.next_leader_identity
    );
}

#[tokio::main]
async fn main() {
    let args: Args = Args::parse();
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info")
    }
    env_logger::builder()
        .format_timestamp(Some(TimestampPrecision::Micros))
        .init();

    let keypair = args
        .keypair_path
        .as_ref()
        .map(|path| Arc::new(read_keypair_file(path).expect("parse kp file")));

    match keypair {
        Some(auth_keypair) => {
            let searcher_client_auth =
                get_searcher_client_auth(
                    args.block_engine_url.as_str(),
                    &auth_keypair,
                ).await
                .expect("Failed to get searcher client with auth. Note: If you don't pass in the auth keypair, we can attempt to connect to the no auth endpoint");
            process_commands(args, searcher_client_auth).await
        }
        None => {
            let searcher_client_no_auth =
                    get_searcher_client_no_auth(
                        args.block_engine_url.as_str(),
                    ).await
                    .expect("Failed to get searcher client with auth. Note: If you don't pass in the auth keypair, we can attempt to connect to the no auth endpoint");
            process_commands(args, searcher_client_no_auth).await
        }
    }
}

async fn process_commands<T>(args: Args, mut client: SearcherServiceClient<T>)
where
    T: tonic::client::GrpcService<tonic::body::BoxBody> + Send + 'static + Clone,
    T::Error: Into<StdError>,
    T::ResponseBody: Body<Data = Bytes> + Send + 'static,
    <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    <T as tonic::client::GrpcService<tonic::body::BoxBody>>::Future: std::marker::Send,
{
    match args.command {
        Commands::NextScheduledLeader => {
            let next_leader = client
                .get_next_scheduled_leader(NextScheduledLeaderRequest {
                    regions: args.regions,
                })
                .await
                .expect("gets next scheduled leader")
                .into_inner();
            info!(
                "Next leader in {} slots in {}.\
                {next_leader:?}",
                next_leader.next_leader_slot - next_leader.current_slot,
                next_leader.next_leader_region
            );
        }
        Commands::ConnectedLeaders => {
            let connected_leaders = client
                .get_connected_leaders_regioned(ConnectedLeadersRegionedRequest {
                    regions: args.regions,
                })
                .await
                .expect("gets connected leaders")
                .into_inner();
            info!("{connected_leaders:?}");
        }
        Commands::ConnectedLeadersInfo { rpc_url } => {
            let connected_leaders_response = client
                .get_connected_leaders_regioned(ConnectedLeadersRegionedRequest {
                    regions: args.regions,
                })
                .await
                .expect("gets connected leaders")
                .into_inner();
            let connected_validators = connected_leaders_response.connected_validators;

            let rpc_client = RpcClient::new(rpc_url);
            let rpc_vote_account_status = rpc_client
                .get_vote_accounts()
                .await
                .expect("gets vote accounts");

            let total_activated_stake: u64 = rpc_vote_account_status
                .current
                .iter()
                .chain(rpc_vote_account_status.delinquent.iter())
                .map(|vote_account| vote_account.activated_stake)
                .sum();

            let mut total_activated_connected_stake = 0;
            for rpc_vote_account_info in rpc_vote_account_status.current {
                if connected_validators
                    .get(&rpc_vote_account_info.node_pubkey)
                    .is_some()
                {
                    total_activated_connected_stake += rpc_vote_account_info.activated_stake;
                    info!(
                        "connected_leader: {}, stake: {:.2}%",
                        rpc_vote_account_info.node_pubkey,
                        (rpc_vote_account_info.activated_stake * 100) as f64
                            / total_activated_stake as f64
                    );
                }
            }
            info!(
                "total stake for block engine: {:.2}%",
                (total_activated_connected_stake * 100) as f64 / total_activated_stake as f64
            );
        }
        Commands::TipAccounts => {
            let tip_accounts = client
                .get_tip_accounts(GetTipAccountsRequest {})
                .await
                .expect("gets connected leaders")
                .into_inner();
            info!("{:?}", tip_accounts);
        }
        Commands::SendBundle {
            rpc_url,
            payer,
            message,
            num_txs,
            lamports,
            tip_account,
        } => {
            let payer_keypair = read_keypair_file(&payer).expect("reads keypair at path");
            let rpc_client = RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed());
            let balance = rpc_client
                .get_balance(&payer_keypair.pubkey())
                .await
                .expect("reads balance");

            info!(
                "payer public key: {:?} lamports: {balance:?}",
                payer_keypair.pubkey(),
            );

            let mut bundle_results_subscription = client
                .subscribe_bundle_results(SubscribeBundleResultsRequest {})
                .await
                .expect("subscribe to bundle results")
                .into_inner();

            // wait for jito-solana leader slot
            let mut is_leader_slot = false;
            while !is_leader_slot {
                let next_leader = client
                    .get_next_scheduled_leader(NextScheduledLeaderRequest {
                        regions: args.regions.clone(),
                    })
                    .await
                    .expect("gets next scheduled leader")
                    .into_inner();
                let num_slots = next_leader.next_leader_slot - next_leader.current_slot;
                is_leader_slot = num_slots <= 2;
                info!(
                    "next jito leader slot in {num_slots} slots in {}",
                    next_leader.next_leader_region
                );
                sleep(Duration::from_millis(500)).await;
            }

            // build + sign the transactions
            let blockhash = rpc_client
                .get_latest_blockhash()
                .await
                .expect("get blockhash");
            let txs: Vec<_> = (0..num_txs)
                .map(|i| {
                    VersionedTransaction::from(Transaction::new_signed_with_payer(
                        &[
                            build_memo(format!("jito bundle {i}: {message}").as_bytes(), &[]),
                            transfer(&payer_keypair.pubkey(), &tip_account, lamports),
                        ],
                        Some(&payer_keypair.pubkey()),
                        &[&payer_keypair],
                        blockhash,
                    ))
                })
                .collect();

            send_bundle_with_confirmation(
                &txs,
                &rpc_client,
                &mut client,
                &mut bundle_results_subscription,
            )
            .await
            .expect("Sending bundle failed");
        }
    }
}

pub async fn print_packet_stream(
    client: &mut SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>,
    mut pending_transactions: Streaming<PendingTxNotification>,
    regions: Vec<String>,
) {
    loop {
        match timeout(Duration::from_secs(5), pending_transactions.next()).await {
            Ok(Some(Ok(notification))) => {
                let transactions: Vec<VersionedTransaction> = notification
                    .transactions
                    .iter()
                    .filter_map(versioned_tx_from_packet)
                    .collect();
                for tx in transactions {
                    info!("tx sig: {:?}", tx.signatures[0]);
                }
            }
            Ok(Some(Err(e))) => {
                info!("error from pending transaction stream: {e:?}");
                break;
            }
            Ok(None) => {
                info!("pending transaction stream closed");
                break;
            }
            Err(_) => {
                print_next_leader_info(client, regions.clone()).await;
            }
        }
    }
}
