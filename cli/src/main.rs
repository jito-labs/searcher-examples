use clap::{Parser, Subcommand};
use env_logger::TimestampPrecision;
use futures_util::StreamExt;
use jito_protos::convert::versioned_tx_from_packet;
use jito_protos::searcher::searcher_service_client::SearcherServiceClient;
use jito_protos::searcher::{NextScheduledLeaderRequest, PendingTxSubscriptionRequest};
use log::{info, LevelFilter};
use searcher_service_client::new_client::{get_searcher_client, AuthInterceptor};
use solana_sdk::signature::read_keypair_file;
use solana_sdk::transaction::VersionedTransaction;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// URL of the block engine
    #[clap(long, env)]
    url: String,

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

    let mut client = get_searcher_client(&args.url, &keypair)
        .await
        .expect("connects to searcher client");

    match args.command {
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
    }
}
