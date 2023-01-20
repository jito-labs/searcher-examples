use clap::{Parser, Subcommand};
use env_logger::TimestampPrecision;
use futures_util::StreamExt;
use jito_protos::convert::versioned_tx_from_packet;
use jito_protos::searcher::searcher_service_client::SearcherServiceClient;
use jito_protos::searcher::{
    ConnectedLeadersRequest, GetTipAccountsRequest, NextScheduledLeaderRequest,
    PendingTxSubscriptionRequest,
};
use log::{info, LevelFilter};
use searcher_service_client::client_with_auth::AuthInterceptor;
use searcher_service_client::get_searcher_client;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::signature::read_keypair_file;
use solana_sdk::system_instruction::transfer;
use solana_sdk::transaction::VersionedTransaction;
use spl_memo::build_memo;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{sleep, timeout};
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;

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
    /// Prints out information on the tip accounts
    GetTipAccounts,
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
        lamports: usize,
    },
}

async fn print_next_leader_info(
    client: &mut SearcherServiceClient<InterceptedService<Channel, AuthInterceptor>>,
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

    env_logger::builder()
        .format_timestamp(Some(TimestampPrecision::Micros))
        .filter_level(LevelFilter::Info)
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
        Commands::GetTipAccounts => {
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
        } => {
            let payer_keypair = read_keypair_file(payer).expect("reads keypair at path");
            let mut is_leader_slot = false;
            while !is_leader_slot {
                let next_leader = client
                    .get_next_scheduled_leader(NextScheduledLeaderRequest {})
                    .await
                    .expect("gets next scheduled leader")
                    .into_inner();
                let num_slots = next_leader.next_leader_slot - next_leader.current_slot;
                is_leader_slot = num_slots <= 2;
                info!("next jito leader slot in {} slots", num_slots);
                sleep(Duration::from_millis(500));
            }

            let blockhash = RpcClient::new(rpc_url)
                .get_latest_blockhash()
                .await
                .expect("get blockhash");

            let tx = (0..num_txs)
                .map(|i| {
                    VersionedTransaction::from(Transaction::new_signed_with_payer(
                        &[
                            build_memo(
                                format!("{}: {:?}", message, mempool_tx.signatures[0].to_string())
                                    .as_bytes(),
                                &[],
                            ),
                            transfer(&payer_keypair.pubkey(), &tip_account, lamports),
                        ],
                        Some(&payer_keypair.pubkey()),
                        &[payer_keypair],
                        blockhash.clone(),
                    ));
                })
                .collect();
        }
    }
}
