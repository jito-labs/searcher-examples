use std::{
    env,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use clap::{Parser, Subcommand};
use env_logger::TimestampPrecision;
use histogram::Histogram;
use jito_protos::searcher::{
    searcher_service_client::SearcherServiceClient, ConnectedLeadersRegionedRequest,
    GetTipAccountsRequest, NextScheduledLeaderRequest, SubscribeBundleResultsRequest,
};
use jito_searcher_client::{
    get_searcher_client_auth, get_searcher_client_no_auth, send_bundle_no_wait,
    token_authenticator::ClientInterceptor,
};
use log::{info, warn};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_metrics::datapoint_info;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{read_keypair_file, Signer},
    system_instruction::transfer,
    transaction::{Transaction, VersionedTransaction},
};
use spl_memo::build_memo;
use tokio::{
    sync::{mpsc, watch, Semaphore},
    time::MissedTickBehavior,
};
use tonic::{
    codegen::{Body, Bytes, InterceptedService, StdError},
    transport::Channel,
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

#[allow(unused)]
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
                if connected_validators.contains_key(&rpc_vote_account_info.node_pubkey) {
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
            let rpc_client = Arc::new(RpcClient::new_with_commitment(
                rpc_url,
                CommitmentConfig::confirmed(),
            ));
            let balance = rpc_client
                .get_balance(&payer_keypair.pubkey())
                .await
                .expect("reads balance");

            info!(
                "payer public key: {:?} lamports: {balance:?}",
                payer_keypair.pubkey(),
            );

            let bundle_results_subscription = client
                .subscribe_bundle_results(SubscribeBundleResultsRequest {})
                .await
                .expect("subscribe to bundle results")
                .into_inner();

            tokio::spawn(async move {
                let mut bundle_results_subscription = bundle_results_subscription;
                loop {
                    match bundle_results_subscription.message().await {
                        Ok(Some(_)) => {}
                        Ok(None) => break,
                        Err(err) => {
                            warn!("Bundle results stream error: {err}");
                            break;
                        }
                    }
                }
            });

            struct SendReport {
                success: bool,
                latency_us: Option<u64>,
            }

            let (metrics_tx, metrics_rx) = mpsc::unbounded_channel::<SendReport>();

            tokio::spawn(async move {
                let mut metrics_rx = metrics_rx;
                let build_latency_histogram = || {
                    Histogram::builder()
                        .maximum_value(60_000_000)
                        .min_resolution(1)
                        .build()
                        .expect("latency histogram")
                };
                let mut window_histogram = build_latency_histogram();
                let total_histogram = build_latency_histogram();

                let mut window_start = Instant::now();
                let mut window_bundles_sent: u64 = 0;
                let mut total_bundles_sent: u64 = 0;
                let mut window_failures: u64 = 0;
                let mut total_failures: u64 = 0;

                while let Some(report) = metrics_rx.recv().await {
                    if report.success {
                        total_bundles_sent += 1;
                        window_bundles_sent += 1;
                        if let Some(latency_us) = report.latency_us {
                            let _ = window_histogram.increment(latency_us, 1);
                            let _ = total_histogram.increment(latency_us, 1);
                        }
                    } else {
                        total_failures += 1;
                        window_failures += 1;
                    }

                    if window_start.elapsed() >= Duration::from_secs(1) {
                        let window_duration = window_start.elapsed();
                        let window_has_data = window_bundles_sent > 0;
                        let total_has_data = total_bundles_sent > 0;

                        let (window_p50, window_p95, window_p99) = if window_has_data {
                            (
                                window_histogram
                                    .percentile(50.0)
                                    .ok()
                                    .map(|bucket| bucket.high() as i64)
                                    .unwrap_or(-1),
                                window_histogram
                                    .percentile(95.0)
                                    .ok()
                                    .map(|bucket| bucket.high() as i64)
                                    .unwrap_or(-1),
                                window_histogram
                                    .percentile(99.0)
                                    .ok()
                                    .map(|bucket| bucket.high() as i64)
                                    .unwrap_or(-1),
                            )
                        } else {
                            (-1, -1, -1)
                        };

                        let (total_p50, total_p95, total_p99) = if total_has_data {
                            (
                                total_histogram
                                    .percentile(50.0)
                                    .ok()
                                    .map(|bucket| bucket.high() as i64)
                                    .unwrap_or(-1),
                                total_histogram
                                    .percentile(95.0)
                                    .ok()
                                    .map(|bucket| bucket.high() as i64)
                                    .unwrap_or(-1),
                                total_histogram
                                    .percentile(99.0)
                                    .ok()
                                    .map(|bucket| bucket.high() as i64)
                                    .unwrap_or(-1),
                            )
                        } else {
                            (-1, -1, -1)
                        };

                        if window_failures > 0 {
                            warn!(
                                "Failed to send {} bundles in the last window",
                                window_failures
                            );
                        }

                        datapoint_info!(
                            "continuous_bundle_sender",
                            ("window_bundles_sent", window_bundles_sent as i64, i64),
                            (
                                "window_duration_ms",
                                window_duration.as_millis() as i64,
                                i64
                            ),
                            ("window_latency_p50_us", window_p50, i64),
                            ("window_latency_p95_us", window_p95, i64),
                            ("window_latency_p99_us", window_p99, i64),
                            ("window_has_data", window_has_data as i64, i64),
                            ("total_bundles_sent", total_bundles_sent as i64, i64),
                            ("total_latency_p50_us", total_p50, i64),
                            ("total_latency_p95_us", total_p95, i64),
                            ("total_latency_p99_us", total_p99, i64),
                            ("total_has_data", total_has_data as i64, i64),
                            ("window_failures", window_failures as i64, i64),
                            ("total_failures", total_failures as i64, i64)
                        );
                        window_start = Instant::now();
                        window_bundles_sent = 0;
                        window_failures = 0;
                        window_histogram = build_latency_histogram();
                    }
                }
            });

            info!("Starting continuous bundle sender");

            let initial_blockhash = rpc_client
                .get_latest_blockhash()
                .await
                .expect("get blockhash");
            let (blockhash_tx, blockhash_rx) = watch::channel(initial_blockhash);
            let rpc_client_for_blockhash = Arc::clone(&rpc_client);
            let _blockhash_updater = tokio::spawn(async move {
                let mut update_interval = tokio::time::interval(Duration::from_secs(1));
                update_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
                loop {
                    update_interval.tick().await;
                    match rpc_client_for_blockhash.get_latest_blockhash().await {
                        Ok(new_blockhash) => {
                            if blockhash_tx.send(new_blockhash).is_err() {
                                break;
                            }
                        }
                        Err(err) => {
                            warn!("Failed to refresh blockhash: {err}");
                        }
                    }
                }
            });

            let mut bundle_counter: u64 = 0;
            let send_semaphore = Arc::new(Semaphore::new(500));

            loop {
                let blockhash = *blockhash_rx.borrow();

                let bundle_index = bundle_counter;
                bundle_counter += 1;
                let txs: Vec<_> = (0..num_txs)
                    .map(|i| {
                        VersionedTransaction::from(Transaction::new_signed_with_payer(
                            &[
                                build_memo(
                                    format!("jito bundle {bundle_index} tx {i}: {message}")
                                        .as_bytes(),
                                    &[],
                                ),
                                transfer(&payer_keypair.pubkey(), &tip_account, lamports),
                            ],
                            Some(&payer_keypair.pubkey()),
                            &[&payer_keypair],
                            blockhash,
                        ))
                    })
                    .collect();

                let mut client_clone = client.clone();
                let metrics_tx = metrics_tx.clone();
                let permit = match send_semaphore.clone().acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => {
                        warn!("Semaphore closed; stopping continuous sender loop");
                        break;
                    }
                };
                tokio::spawn(async move {
                    let _permit = permit;
                    let send_started = Instant::now();
                    match send_bundle_no_wait(&txs, &mut client_clone).await {
                        Ok(_) => {
                            let latency_us = send_started.elapsed().as_micros() as u64;
                            let _ = metrics_tx.send(SendReport {
                                success: true,
                                latency_us: Some(latency_us),
                            });
                        }
                        Err(err) => {
                            warn!("Failed to send bundle: {err}");
                            let _ = metrics_tx.send(SendReport {
                                success: false,
                                latency_us: None,
                            });
                        }
                    }
                });
            }
        }
    }
}
