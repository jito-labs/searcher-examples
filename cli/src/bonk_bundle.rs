use std::{path::PathBuf, time::Duration, fs::File, io::{self, BufRead}};
use clap::Subcommand;
use log::info;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{read_keypair_file, Signer},
    transaction::{Transaction, VersionedTransaction},
};
use spl_memo::build_memo;
use spl_token::instruction::transfer as spl_token_transfer;
use tokio::time::sleep;
use jito_protos::searcher::{
    searcher_service_client::SearcherServiceClient,
    NextScheduledLeaderRequest, SubscribeBundleResultsRequest,
};
use jito_searcher_client::send_bundle_with_confirmation;
use tonic::codegen::{Body, Bytes, StdError};

#[derive(Debug, Subcommand)]
pub enum BonkBundleCommand {
    /// Sends a bundle of BONK transfers based on addresses and amounts in addys-bonk.txt
    SendBonkBundle {
        /// RPC URL
        #[clap(long, required = true)]
        rpc_url: String,
        /// Filepath to keypair that can afford the transaction payments
        #[clap(long, required = true)]
        payer: PathBuf,
        /// One of the tip accounts
        #[clap(long, required = true)]
        tip_account: Pubkey,
        /// Amount of lamports to tip in each bundle
        #[clap(long, required = true)]
        tip_lamports: u64,
        /// BONK token account of the payer
        #[clap(long, required = true)]
        bonk_token_account: Pubkey,
        /// BONK token mint address
        #[clap(long, required = true)]
        bonk_mint: Pubkey,
    },
}

pub async fn handle_bonk_bundle_command<T>(
    command: &BonkBundleCommand,
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
        BonkBundleCommand::SendBonkBundle {
            rpc_url,
            payer,
            tip_account,
            tip_lamports,
            bonk_token_account,
            bonk_mint,
        } => {
            send_bonk_bundle(
                client,
                rpc_url,
                payer,
                tip_account,
                *tip_lamports,
                bonk_token_account,
                bonk_mint,
                regions,
            )
            .await;
        }
    }
}

async fn send_bonk_bundle<T>(
    client: &mut SearcherServiceClient<T>,
    rpc_url: &str,
    payer_path: &PathBuf,
    tip_account: &Pubkey,
    tip_lamports: u64,
    bonk_token_account: &Pubkey,
    bonk_mint: &Pubkey,
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

    // Read transfers from addys-bonk.txt
    let transfers = read_transfers_from_file("addys-bonk.txt").expect("read transfers from file");

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
                let mut instructions = vec![
                    build_memo(
                        format!("jito BONK transfer bundle {}: {}", bundle_index, tx_index).as_bytes(),
                        &[],
                    ),
                    spl_token_transfer(
                        &spl_token::id(),
                        bonk_token_account,
                        recipient,
                        &payer_keypair.pubkey(),
                        &[&payer_keypair.pubkey()],
                        *amount,
                    )
                    .unwrap(),
                ];

                // Add tip only to the last transaction in the bundle
                if tx_index == bundle.len() - 1 {
                    instructions.push(
                        solana_sdk::system_instruction::transfer(&payer_keypair.pubkey(), tip_account, tip_lamports)
                    );
                }

                VersionedTransaction::from(Transaction::new_signed_with_payer(
                    &instructions,
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
        let amount = parts[1].parse::<u64>().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        transfers.push((address, amount));
    }

    Ok(transfers)
}