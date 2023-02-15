use std::{env, str::FromStr, sync::Arc, time::Duration};

use clap::{Parser, Subcommand};
use env_logger::TimestampPrecision;
use futures_util::StreamExt;
use jito_protos::{
    convert::{versioned_tx_from_packet},
    searcher::{
        searcher_service_client::SearcherServiceClient, ConnectedLeadersRequest,
        GetTipAccountsRequest, NextScheduledLeaderRequest, PendingTxSubscriptionRequest,
    },
};
use jito_searcher_client::{
    build_and_send_bundle, get_searcher_client, token_authenticator::ClientInterceptor,
};
use log::{info};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{read_keypair_file, Signer},
    system_instruction::transfer,
    transaction::{Transaction, VersionedTransaction},
};
use spl_memo::build_memo;
use tokio::time::{timeout};
use tonic::{codegen::InterceptedService, transport::Channel};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// URL of the block engine
    #[clap(long, env)]
    block_engine_url: String,

    /// Filepath to a keypair that's authenticated with the block engine
    #[clap(long, env)]
    keypair_path: String,

    /// Subcommand to run
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Subscribe to slot updates from Geyser
    MempoolAccounts {
        /// A space-separated list of accounts to subscribe to
        #[clap(required = true)]
        accounts: Vec<String>,
    },
    /// Print out information on the next scheduled leader
    NextScheduledLeader,
    /// Prints out information on connected leaders
    ConnectedLeaders,
    /// Prints out connected leaders with their leader slot percentage
    ConnectedLeadersInfo {
        #[clap(long, required = true)]
        rpc_url: String,
    },
    /// Prints out information on the tip accounts
    TipAccounts,
    /// Sends a 1 lamport bundle
    SendBundle {
        /// RPC URL
        #[clap(long, required = true)]
        rpc_url: String,
        /// Filepath to keypair that can afford the transaction payments with 1 lamport tip
        #[clap(long, required = true)]
        payer: String,
        /// Message you'd like the bundle to say
        #[clap(long, required = true)]
        message: String,
        /// Number of transactions in the bundle (must be <= 5)
        #[clap(long, required = true)]
        num_txs: usize,
        /// Amount of lamports to tip in each transaction
        #[clap(long, required = true)]
        lamports: u64,
        /// One of the tip accounts
        #[clap(long, required = true)]
        tip_account: String,
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

    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info")
    }
    env_logger::builder()
        .format_timestamp(Some(TimestampPrecision::Micros))
        .init();

    let keypair = Arc::new(read_keypair_file(args.keypair_path).expect("reads keypair at path"));

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
                "waiting for pending transactions for accounts: {:?}",
                accounts
            );
            let mut pending_transactions = client
                .subscribe_pending_transactions(PendingTxSubscriptionRequest { accounts })
                .await
                .expect("subscribes to pending transactions")
                .into_inner();

            print_next_leader_info(&mut client).await;

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
                        print_next_leader_info(&mut client).await;
                    }
                }
            }
        }
        Commands::SendBundle {
            rpc_url,
            payer,
            message,
            num_txs,
            lamports,
            tip_account,
        } => {
            // build + sign the transactions
            let rpc_client = RpcClient::new(rpc_url.clone());
            let blockhash = rpc_client
                .get_latest_blockhash()
                .await
                .expect("get blockhash");
            let payer_keypair = read_keypair_file(payer.clone()).expect("reads keypair at path");
            let txs: Vec<_> = (0..num_txs)
                .map(|i| {
                    VersionedTransaction::from(Transaction::new_signed_with_payer(
                        &[
                            build_memo(format!("jito bundle {}: {}", i, message).as_bytes(), &[]),
                            transfer(
                                &payer_keypair.pubkey(),
                                &Pubkey::from_str(tip_account.as_str()).unwrap(),
                                lamports,
                            ),
                        ],
                        Some(&payer_keypair.pubkey()),
                        &[&payer_keypair],
                        blockhash.clone(),
                    ))
                })
                .collect();

            build_and_send_bundle(
                rpc_url.clone(),
                txs,
                payer.clone(),
                lamports,
                tip_account,
                &mut client,
            )
            .await;
        }
    }
}
