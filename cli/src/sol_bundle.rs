use std::{path::PathBuf, time::Duration, fs::File, io::{self, BufRead}};
use clap::Subcommand;
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
use tokio::time::sleep;
use jito_protos::searcher::{
    searcher_service_client::SearcherServiceClient,
    NextScheduledLeaderRequest, SubscribeBundleResultsRequest,
};
use jito_searcher_client::send_bundle_with_confirmation;
use tonic::codegen::{Body, Bytes, StdError};

#[derive(Debug, Subcommand)]
pub enum SolBundleCommand {
    /// Sends a bundle of SOL transfers based on addresses and amounts in addys.txt
    SendSolBundle {
        /// RPC URL
        #[clap(long, required = true)]
        rpc_url: String,
        /// Filepath to keypair that can afford the transaction payments
        #[clap(long, required = true)]
        payer: PathBuf,
        /// One of the tip accounts
        #[clap(long, required = true)]
        tip_account: Pubkey,
        /// Amount of lamports to tip in each transaction
        #[clap(long, required = true)]
        tip_lamports: u64,
    },
}

pub async fn handle_sol_bundle_command<T>(
    command: &SolBundleCommand,
    client: &mut SearcherServiceClient<T>,
    regions: Vec<String>,
) where
    T: tonic::client::GrpcService<tonic::body::BoxBody> + Send + Sync + Clone + 'static,
    T::Error: Into<StdError>,
    T::ResponseBody: Body<Data = Bytes> + Send + 'static,
    <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    T::Future: Send + 'static,
{
    match command {
        SolBundleCommand::SendSolBundle {
            rpc_url,
            payer,
            tip_account,
            tip_lamports,
        } => {
            send_sol_bundle(
                client,
                rpc_url,
                payer,
                tip_account,
                *tip_lamports,
                regions,
            )
            .await;
        }
    }
}

async fn send_sol_bundle<T>(
    client: &mut SearcherServiceClient<T>,
    rpc_url: &str,
    payer_path: &PathBuf,
    tip_account: &Pubkey,
    tip_lamports: u64,
    regions: Vec<String>,
) where
    T: tonic::client::GrpcService<tonic::body::BoxBody> + Send + Sync + Clone + 'static,
    T::Error: Into<StdError>,
    T::ResponseBody: Body<Data = Bytes> + Send + 'static,
    <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    T::Future: Send + 'static,
{
    let payer_keypair = read_keypair_file(payer_path).expect("reads keypair at path");
    let rpc_client = RpcClient::new_with_commitment(rpc_url.to_string(), CommitmentConfig::confirmed());

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

    // Read transfers from addys.txt
    let transfers = read_transfers_from_file("addys.txt").expect("read transfers from file");

    // Split transfers into bundles of 5 transactions
    let bundles: Vec<Vec<(Pubkey, u64)>> = transfers.chunks(5).map(|chunk| chunk.to_vec()).collect();

    for (bundle_index, bundle) in bundles.iter().enumerate() {
        // Wait for jito-solana leader slot
        let mut is_leader_slot = false;
        while !is_leader_slot {
            let next_leader = client
                .get_next_scheduled_leader(NextScheduledLeaderRequest { regions: regions.clone() })
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

        // Build and sign the transactions
        let blockhash = rpc_client
            .get_latest_blockhash()
            .await
            .expect("get blockhash");

        let txs: Vec<VersionedTransaction> = bundle
            .iter()
            .enumerate()
            .map(|(tx_index, (recipient, amount))| {
                VersionedTransaction::from(Transaction::new_signed_with_payer(
                    &[
                        build_memo(
                            format!("jito SOL transfer bundle {}: {}", bundle_index, tx_index).as_bytes(),
                            &[],
                        ),
                        transfer(&payer_keypair.pubkey(), recipient, *amount),
                        transfer(&payer_keypair.pubkey(), tip_account, tip_lamports),
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
            client,
            &mut bundle_results_subscription,
        )
        .await
        .expect("Sending bundle failed");

        info!("Bundle {} sent successfully", bundle_index);
    }
}

fn read_transfers_from_file(filename: &str) -> io::Result<Vec<(Pubkey, u64)>> {
    let file = File::open(filename)?;
    let reader = io::BufReader::new(file);
    let mut transfers = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() != 2 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid line format"));
        }

        let address = parts[0].parse::<Pubkey>().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let amount = (parts[1].parse::<f64>().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))? * 1e9) as u64;

        transfers.push((address, amount));
    }

    Ok(transfers)
}