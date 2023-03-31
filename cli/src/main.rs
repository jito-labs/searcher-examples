use std::path::PathBuf;
use std::{env, sync::Arc, time::Duration};

use clap::{Parser, Subcommand};
use env_logger::TimestampPrecision;
use futures_util::StreamExt;
use jito_protos::{
    convert::versioned_tx_from_packet,
    searcher::{
        mempool_subscription, searcher_service_client::SearcherServiceClient,
        ConnectedLeadersRequest, GetTipAccountsRequest, MempoolSubscription,
        NextScheduledLeaderRequest, PendingTxNotification, ProgramSubscriptionV0,
        SubscribeBundleResultsRequest, WriteLockedAccountSubscriptionV0,
    },
};
use jito_searcher_client::{
    get_searcher_client, send_bundle_with_confirmation, token_authenticator::ClientInterceptor,
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
use tonic::{codegen::InterceptedService, transport::Channel, Streaming};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// URL of the block engine
    #[arg(long, env)]
    block_engine_url: String,

    /// Filepath to a keypair that's authenticated with the block engine
    #[arg(long, env)]
    auth_keypair_path: PathBuf,

    /// Subcommand to run
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Subscribe to mempool accounts
    MempoolAccounts {
        /// A comma-separated list of accounts to subscribe to
        #[arg(long, value_delimiter = ',', required = true)]
        accounts: Vec<String>,
    },
    /// Subscribe to mempool by program IDs
    MempoolPrograms {
        /// A comma-separated list of programs to subscribe to
        #[arg(long, value_delimiter = ',', required = true)]
        programs: Vec<String>,
    },
    /// Print out information on the next scheduled leader
    NextScheduledLeader,
    /// Prints out information on connected leaders
    ConnectedLeaders,
    /// Prints out connected leaders with their leader slot percentage
    ConnectedLeadersInfo {
        #[arg(long)]
        rpc_url: String,
    },
    /// Prints out information on the tip accounts
    TipAccounts,
    /// Sends a 1 lamport bundle
    SendBundle {
        /// RPC URL
        #[arg(long)]
        rpc_url: String,
        /// Filepath to keypair that can afford the transaction payments with 1 lamport tip
        #[arg(long)]
        payer_keypair_path: PathBuf,
        /// Message you'd like the bundle to say
        #[arg(long)]
        message: String,
        /// Number of transactions in the bundle (must be <= 5)
        #[arg(long)]
        num_txs: usize,
        /// Amount of lamports to tip in each transaction
        #[arg(long)]
        lamports: u64,
        /// One of the tip accounts
        #[arg(long)]
        tip_account: Pubkey,
    },
}

async fn print_next_leader_info(
    client: &mut SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>,
) {
    let next_leader = client
        .get_next_scheduled_leader(NextScheduledLeaderRequest {})
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
    dbg!(&args);
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info")
    }
    env_logger::builder()
        .format_timestamp(Some(TimestampPrecision::Micros))
        .init();

    let keypair =
        Arc::new(read_keypair_file(args.auth_keypair_path).expect("reads keypair at path"));

    let mut client = get_searcher_client(&args.block_engine_url, &keypair)
        .await
        .expect("connects to searcher client");

    match args.command {
        Commands::NextScheduledLeader => {
            let next_leader = client
                .get_next_scheduled_leader(NextScheduledLeaderRequest {})
                .await
                .expect("gets next scheduled leader")
                .into_inner();
            info!("{:?}", next_leader);
        }
        Commands::ConnectedLeaders => {
            let connected_leaders = client
                .get_connected_leaders(ConnectedLeadersRequest {})
                .await
                .expect("gets connected leaders")
                .into_inner();
            info!("{:?}", connected_leaders);
        }
        Commands::ConnectedLeadersInfo { rpc_url } => {
            let connected_leaders_response = client
                .get_connected_leaders(ConnectedLeadersRequest {})
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
                if let Some(_) = connected_validators.get(&rpc_vote_account_info.node_pubkey) {
                    total_activated_connected_stake += rpc_vote_account_info.activated_stake;
                    info!(
                        "connected_leader: {}, stake: {:.2}%",
                        rpc_vote_account_info.node_pubkey,
                        rpc_vote_account_info.activated_stake as f64 * 100f64
                            / total_activated_stake as f64
                    );
                }
            }
            info!(
                "total stake for block engine: {:.2}%",
                total_activated_connected_stake as f64 * 100f64 / total_activated_stake as f64
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
        Commands::MempoolAccounts { accounts } => {
            info!(
                "waiting for mempool transactions that write-locks accounts: {:?}",
                accounts
            );
            let pending_transactions = client
                .subscribe_mempool(MempoolSubscription {
                    msg: Some(mempool_subscription::Msg::WlaV0Sub(
                        WriteLockedAccountSubscriptionV0 { accounts },
                    )),
                })
                .await
                .expect("subscribes to pending transactions by write-locked accounts")
                .into_inner();

            print_next_leader_info(&mut client).await;
            print_packet_stream(&mut client, pending_transactions).await;
        }
        Commands::MempoolPrograms { programs } => {
            info!("waiting for mempool transactions that mention programs: {programs:?}");
            let pending_transactions = client
                .subscribe_mempool(MempoolSubscription {
                    msg: Some(mempool_subscription::Msg::ProgramV0Sub(
                        ProgramSubscriptionV0 { programs },
                    )),
                })
                .await
                .expect("subscribes to pending transactions by program id")
                .into_inner();

            print_next_leader_info(&mut client).await;
            print_packet_stream(&mut client, pending_transactions).await;
        }
        Commands::SendBundle {
            rpc_url,
            payer_keypair_path,
            message,
            num_txs,
            lamports,
            tip_account,
        } => {
            let payer_keypair =
                read_keypair_file(payer_keypair_path).expect("reads keypair at path");
            let rpc_client = RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed());
            let balance = rpc_client
                .get_balance(&payer_keypair.pubkey())
                .await
                .expect("reads balance");

            info!(
                "payer public key: {:?} lamports: {:?}",
                payer_keypair.pubkey(),
                balance
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
                    .get_next_scheduled_leader(NextScheduledLeaderRequest {})
                    .await
                    .expect("gets next scheduled leader")
                    .into_inner();
                let num_slots = next_leader.next_leader_slot - next_leader.current_slot;
                is_leader_slot = num_slots <= 2;
                info!("next jito leader slot in {num_slots} slots");
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

async fn print_packet_stream(
    client: &mut SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>,
    mut pending_transactions: Streaming<PendingTxNotification>,
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
                info!("error from pending transaction stream: {:?}", e);
                break;
            }
            Ok(None) => {
                info!("pending transaction stream closed");
                break;
            }
            Err(_) => {
                print_next_leader_info(client).await;
            }
        }
    }
}
