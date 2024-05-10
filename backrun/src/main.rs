mod event_loops;

use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    path::PathBuf,
    result,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use clap::Parser;
use env_logger::TimestampPrecision;
use histogram::Histogram;
use jito_protos::{
    bundle::BundleResult,
    convert::versioned_tx_from_packet,
    searcher::{
        searcher_service_client::SearcherServiceClient, ConnectedLeadersRequest,
        NextScheduledLeaderRequest, PendingTxNotification, SendBundleResponse,
    },
};
use jito_searcher_client::{
    get_searcher_client_auth, get_searcher_client_no_auth, send_bundle_no_wait,
    BlockEngineConnectionError,
};
use log::*;
use rand::{rngs::ThreadRng, thread_rng, Rng};
use solana_client::{
    client_error::ClientError,
    nonblocking::{pubsub_client::PubsubClientError, rpc_client::RpcClient},
    rpc_response,
    rpc_response::RpcBlockUpdate,
};
use solana_metrics::{datapoint_info, set_host_id};
use solana_sdk::{
    clock::Slot,
    commitment_config::{CommitmentConfig, CommitmentLevel},
    hash::Hash,
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair, Signature, Signer},
    system_instruction::transfer,
    transaction::{Transaction, VersionedTransaction},
};
use spl_memo::build_memo;
use thiserror::Error;
use tokio::{
    runtime::Builder,
    sync::mpsc::{channel, Receiver},
    time::interval,
};
use tonic::{
    codegen::{Body, Bytes, StdError},
    Response, Status,
};

use crate::event_loops::{
    block_subscribe_loop, bundle_results_loop, pending_tx_loop, slot_subscribe_loop,
};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// URL of the block engine.
    /// See: https://jito-labs.gitbook.io/mev/searcher-resources/block-engine#connection-details
    #[arg(long, env)]
    block_engine_url: String,

    /// Account pubkeys to backrun
    #[arg(long, env)]
    backrun_accounts: Vec<Pubkey>,

    /// Path to keypair file used to sign and pay for transactions
    #[arg(long, env)]
    payer_keypair: PathBuf,

    /// Path to keypair file used to authenticate with the Jito Block Engine
    /// See: https://jito-labs.gitbook.io/mev/searcher-resources/getting-started#block-engine-api-key
    #[arg(long, env)]
    auth_keypair: Option<PathBuf>,

    /// RPC Websocket URL.
    /// See: https://solana.com/docs/rpc/websocket
    /// Note that this RPC server must have --rpc-pubsub-enable-block-subscription enabled
    #[arg(long, env)]
    pubsub_url: String,

    /// RPC HTTP URL.
    #[arg(long, env)]
    rpc_url: String,

    /// Message to pass into the memo program as part of a bundle.
    #[arg(long, env, default_value = "jito backrun")]
    message: String,

    /// Tip payment program public key
    /// See: https://jito-foundation.gitbook.io/mev/mev-payment-and-distribution/on-chain-addresses
    #[arg(long, env)]
    tip_program_id: Pubkey,

    /// Comma-separated list of regions to request cross-region data from.
    /// If no region specified, then default to the currently connected block engine's region.
    /// Details: https://jito-labs.gitbook.io/mev/searcher-services/recommendations#cross-region
    /// Available regions: https://jito-labs.gitbook.io/mev/searcher-resources/block-engine#connection-details
    #[arg(long, env, value_delimiter = ',')]
    regions: Vec<String>,

    /// Subscribe and print bundle results.
    #[arg(long, env, default_value_t = true)]
    subscribe_bundle_results: bool,
}

#[derive(Debug, Error)]
enum BackrunError {
    #[error("TonicError {0}")]
    TonicError(#[from] tonic::transport::Error),
    #[error("GrpcError {0}")]
    GrpcError(#[from] Status),
    #[error("RpcError {0}")]
    RpcError(#[from] ClientError),
    #[error("PubSubError {0}")]
    PubSubError(#[from] PubsubClientError),
    #[error("BlockEngineConnectionError {0}")]
    BlockEngineConnectionError(#[from] BlockEngineConnectionError),
    #[error("Shutdown")]
    Shutdown,
}

#[derive(Clone)]
struct BundledTransactions {
    mempool_txs: Vec<VersionedTransaction>,
    backrun_txs: Vec<VersionedTransaction>,
}

#[derive(Default)]
struct BlockStats {
    bundles_sent: Vec<(
        BundledTransactions,
        tonic::Result<Response<SendBundleResponse>>,
    )>,
    send_elapsed: u64,
    send_rt_per_packet: Histogram,
}

type Result<T> = result::Result<T, BackrunError>;

fn build_bundles(
    pending_tx_notification: PendingTxNotification,
    keypair: &Keypair,
    blockhash: &Hash,
    tip_accounts: &[Pubkey],
    rng: &mut ThreadRng,
    message: &str,
) -> Vec<BundledTransactions> {
    pending_tx_notification
        .transactions
        .into_iter()
        .filter_map(|packet| {
            let mempool_tx = versioned_tx_from_packet(&packet)?;
            let tip_account = tip_accounts[rng.gen_range(0..tip_accounts.len())];

            let backrun_tx = VersionedTransaction::from(Transaction::new_signed_with_payer(
                &[
                    build_memo(
                        format!("{}: {:?}", message, mempool_tx.signatures[0].to_string())
                            .as_bytes(),
                        &[],
                    ),
                    transfer(&keypair.pubkey(), &tip_account, 10_000),
                ],
                Some(&keypair.pubkey()),
                &[keypair],
                *blockhash,
            ));
            Some(BundledTransactions {
                mempool_txs: vec![mempool_tx],
                backrun_txs: vec![backrun_tx],
            })
        })
        .collect()
}

async fn send_bundles<T>(
    searcher_client: &mut SearcherServiceClient<T>,
    bundles: &[BundledTransactions],
) -> Result<Vec<result::Result<Response<SendBundleResponse>, Status>>>
where
    T: tonic::client::GrpcService<tonic::body::BoxBody> + Send + 'static + Clone,
    T::Error: Into<StdError>,
    T::ResponseBody: Body<Data = Bytes> + Send + 'static,
    <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    <T as tonic::client::GrpcService<tonic::body::BoxBody>>::Future: std::marker::Send,
{
    let mut futs = Vec::with_capacity(bundles.len());
    for b in bundles {
        let mut searcher_client = searcher_client.clone();
        let txs = b
            .mempool_txs
            .clone()
            .into_iter()
            .chain(b.backrun_txs.clone().into_iter())
            .collect::<Vec<VersionedTransaction>>();
        let task =
            tokio::spawn(async move { send_bundle_no_wait(&txs, &mut searcher_client).await });
        futs.push(task);
    }

    let responses = futures_util::future::join_all(futs).await;
    let send_bundle_responses = responses.into_iter().map(|r| r.unwrap()).collect();
    Ok(send_bundle_responses)
}

fn generate_tip_accounts(tip_program_pubkey: &Pubkey) -> Vec<Pubkey> {
    let tip_pda_0 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_0"], tip_program_pubkey).0;
    let tip_pda_1 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_1"], tip_program_pubkey).0;
    let tip_pda_2 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_2"], tip_program_pubkey).0;
    let tip_pda_3 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_3"], tip_program_pubkey).0;
    let tip_pda_4 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_4"], tip_program_pubkey).0;
    let tip_pda_5 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_5"], tip_program_pubkey).0;
    let tip_pda_6 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_6"], tip_program_pubkey).0;
    let tip_pda_7 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_7"], tip_program_pubkey).0;

    vec![
        tip_pda_0, tip_pda_1, tip_pda_2, tip_pda_3, tip_pda_4, tip_pda_5, tip_pda_6, tip_pda_7,
    ]
}

async fn maintenance_tick<T>(
    searcher_client: &mut SearcherServiceClient<T>,
    rpc_client: &RpcClient,
    leader_schedule: &mut HashMap<Pubkey, HashSet<Slot>>,
    blockhash: &mut Hash,
    regions: Vec<String>,
) -> Result<()>
where
    T: tonic::client::GrpcService<tonic::body::BoxBody> + Send + 'static + Clone,
    T::Error: Into<StdError>,
    T::ResponseBody: Body<Data = Bytes> + Send + 'static,
    <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    <T as tonic::client::GrpcService<tonic::body::BoxBody>>::Future: std::marker::Send,
{
    *blockhash = rpc_client
        .get_latest_blockhash_with_commitment(CommitmentConfig {
            commitment: CommitmentLevel::Confirmed,
        })
        .await?
        .0;
    let new_leader_schedule = searcher_client
        .get_connected_leaders(ConnectedLeadersRequest {})
        .await?
        .into_inner()
        .connected_validators
        .iter()
        .fold(HashMap::new(), |mut hmap, (pubkey, slot_list)| {
            hmap.insert(
                Pubkey::from_str(pubkey).unwrap(),
                slot_list.slots.iter().cloned().collect(),
            );
            hmap
        });
    if new_leader_schedule != *leader_schedule {
        info!("connected_validators: {:?}", new_leader_schedule.keys());
        *leader_schedule = new_leader_schedule;
    }

    let next_scheduled_leader = searcher_client
        .get_next_scheduled_leader(NextScheduledLeaderRequest { regions })
        .await?
        .into_inner();
    info!(
        "next_scheduled_leader: {} in {} slots from {}",
        next_scheduled_leader.next_leader_identity,
        next_scheduled_leader.next_leader_slot - next_scheduled_leader.current_slot,
        next_scheduled_leader.next_leader_region
    );

    Ok(())
}

fn print_block_stats(
    block_stats: &mut HashMap<Slot, BlockStats>,
    block: rpc_response::Response<RpcBlockUpdate>,
    leader_schedule: &HashMap<Pubkey, HashSet<Slot>>,
    block_signatures: &mut HashMap<Slot, HashSet<Signature>>,
) {
    const KEEP_SIGS_SLOTS: u64 = 20;

    if let Some(stats) = block_stats.get(&block.context.slot) {
        datapoint_info!(
            "bundles-sent",
            ("slot", block.context.slot, i64),
            ("bundles", stats.bundles_sent.len(), i64),
            ("total_send_elapsed_us", stats.send_elapsed, i64),
            (
                "sent_rt_pp_min",
                stats.send_rt_per_packet.minimum().unwrap_or_default(),
                i64
            ),
            (
                "sent_rt_pp_max",
                stats.send_rt_per_packet.maximum().unwrap_or_default(),
                i64
            ),
            (
                "sent_rt_pp_avg",
                stats.send_rt_per_packet.mean().unwrap_or_default(),
                i64
            ),
            (
                "sent_rt_pp_p50",
                stats
                    .send_rt_per_packet
                    .percentile(50.0)
                    .unwrap_or_default(),
                i64
            ),
            (
                "sent_rt_pp_p90",
                stats
                    .send_rt_per_packet
                    .percentile(90.0)
                    .unwrap_or_default(),
                i64
            ),
            (
                "sent_rt_pp_p95",
                stats
                    .send_rt_per_packet
                    .percentile(95.0)
                    .unwrap_or_default(),
                i64
            ),
        );
    }

    let maybe_leader = leader_schedule
        .iter()
        .find(|(_, slots)| slots.contains(&block.context.slot))
        .map(|(leader, _)| leader);

    if let Some(b) = &block.value.block {
        if let Some(sigs) = &b.signatures {
            let block_signatures: HashSet<Signature> = sigs
                .iter()
                .map(|s| Signature::from_str(s).unwrap())
                .collect();

            // bundles that were sent before or during this slot
            #[allow(clippy::type_complexity)]
            let bundles_sent_before_slot: HashMap<
                Slot,
                &[(
                    BundledTransactions,
                    tonic::Result<Response<SendBundleResponse>>,
                )],
            > = block_stats
                .iter()
                .filter(|(slot, _)| **slot <= block.context.slot)
                .map(|(slot, stats)| (*slot, stats.bundles_sent.as_ref()))
                .collect();

            if let Some(leader) = maybe_leader {
                // number of bundles sent before or during this slot
                let num_bundles_sent: usize = bundles_sent_before_slot
                    .values()
                    .map(|bundles_sent| bundles_sent.len())
                    .sum();

                // number of bundles where sending returned ok
                let num_bundles_sent_ok: usize = bundles_sent_before_slot
                    .values()
                    .map(|bundles_sent| {
                        bundles_sent
                            .iter()
                            .filter(|(_, send_response)| send_response.is_ok())
                            .count()
                    })
                    .sum();

                // a list of all bundles landed this slot that were sent before or during this slot
                let bundles_landed: Vec<(Slot, &BundledTransactions)> = bundles_sent_before_slot
                    .iter()
                    .flat_map(|(slot, bundles_sent_slot)| {
                        bundles_sent_slot
                            .iter()
                            .filter(|(_, send_response)| send_response.is_ok())
                            .filter_map(|(bundle_sent, _)| {
                                if bundle_sent
                                    .backrun_txs
                                    .iter()
                                    .chain(bundle_sent.mempool_txs.iter())
                                    .all(|tx| block_signatures.contains(&tx.signatures[0]))
                                {
                                    Some((*slot, bundle_sent))
                                } else {
                                    None
                                }
                            })
                    })
                    .collect();

                let mempool_txs_landed_no_bundle: Vec<(Slot, &BundledTransactions)> =
                    bundles_sent_before_slot
                        .iter()
                        .flat_map(|(slot, bundles_sent_slot)| {
                            bundles_sent_slot
                                .iter()
                                .filter(|(_, send_response)| send_response.is_ok())
                                .filter_map(|(bundle_sent, _)| {
                                    if bundle_sent
                                        .mempool_txs
                                        .iter()
                                        .any(|tx| block_signatures.contains(&tx.signatures[0]))
                                        && !bundle_sent
                                            .backrun_txs
                                            .iter()
                                            .any(|tx| block_signatures.contains(&tx.signatures[0]))
                                    {
                                        Some((*slot, bundle_sent))
                                    } else {
                                        None
                                    }
                                })
                        })
                        .collect();

                // find the min and max distance from when the bundle was sent to what block it landed in
                let min_bundle_send_slot = bundles_landed
                    .iter()
                    .map(|(slot, _)| *slot)
                    .min()
                    .unwrap_or(0);
                let max_bundle_send_slot = bundles_landed
                    .iter()
                    .map(|(slot, _)| *slot)
                    .max()
                    .unwrap_or(0);

                datapoint_info!(
                    "leader-bundle-stats",
                    ("slot", block.context.slot, i64),
                    ("leader", leader.to_string(), String),
                    ("block_txs", block_signatures.len(), i64),
                    ("num_bundles_sent", num_bundles_sent, i64),
                    ("num_bundles_sent_ok", num_bundles_sent_ok, i64),
                    (
                        "num_bundles_sent_err",
                        num_bundles_sent - num_bundles_sent_ok,
                        i64
                    ),
                    ("num_bundles_landed", bundles_landed.len(), i64),
                    (
                        "num_bundles_dropped",
                        num_bundles_sent - bundles_landed.len(),
                        i64
                    ),
                    ("min_bundle_send_slot", min_bundle_send_slot, i64),
                    ("max_bundle_send_slot", max_bundle_send_slot, i64),
                    (
                        "mempool_txs_landed_no_bundle",
                        mempool_txs_landed_no_bundle.len(),
                        i64
                    ),
                );

                // leaders last slot, clear everything out
                // might mess up metrics if leader doesn't produce a last slot or there's lots of slots
                // close to each other
                if block.context.slot % 4 == 3 {
                    block_stats.clear();
                }
            } else {
                // figure out how many transactions in bundles landed in slots other than our leader
                let num_mempool_txs_landed: usize = bundles_sent_before_slot
                    .values()
                    .map(|bundles| {
                        bundles
                            .iter()
                            .filter(|(bundle, _)| {
                                bundle
                                    .mempool_txs
                                    .iter()
                                    .any(|tx| block_signatures.contains(&tx.signatures[0]))
                            })
                            .count()
                    })
                    .sum();
                if num_mempool_txs_landed > 0 {
                    datapoint_info!(
                        "non-leader-bundle-stats",
                        ("slot", block.context.slot, i64),
                        ("mempool_txs_landed", num_mempool_txs_landed, i64),
                    );
                }
            }
        }
    }

    if let Some(b) = &block.value.block {
        if let Some(sigs) = &b.signatures {
            block_signatures.insert(
                block.context.slot,
                sigs.iter()
                    .map(|s| Signature::from_str(s).unwrap())
                    .collect(),
            );
        }
    }

    // throw away signatures for slots > KEEP_SIGS_SLOTS old
    block_signatures.retain(|slot, _| *slot > block.context.slot - KEEP_SIGS_SLOTS);
}

#[allow(clippy::too_many_arguments)]
async fn run_searcher_loop<T>(
    mut searcher_client: SearcherServiceClient<T>,
    keypair: &Keypair,
    rpc_url: String,
    regions: Vec<String>,
    message: String,
    tip_program_pubkey: Pubkey,
    mut slot_receiver: Receiver<Slot>,
    mut block_receiver: Receiver<rpc_response::Response<RpcBlockUpdate>>,
    mut bundle_results_receiver: Receiver<BundleResult>,
    mut pending_tx_receiver: Receiver<PendingTxNotification>,
) -> Result<()>
where
    T: tonic::client::GrpcService<tonic::body::BoxBody> + Send + 'static + Clone,
    T::Error: Into<StdError>,
    T::ResponseBody: Body<Data = Bytes> + Send + 'static,
    <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    <T as tonic::client::GrpcService<tonic::body::BoxBody>>::Future: std::marker::Send,
{
    let mut leader_schedule: HashMap<Pubkey, HashSet<Slot>> = HashMap::new();
    let mut block_stats: HashMap<Slot, BlockStats> = HashMap::new();
    let mut block_signatures: HashMap<Slot, HashSet<Signature>> = HashMap::new();

    let mut rng = thread_rng();

    let tip_accounts = generate_tip_accounts(&tip_program_pubkey);
    info!("tip accounts: {:?}", tip_accounts);

    let rpc_client = RpcClient::new(rpc_url);
    let mut blockhash = rpc_client
        .get_latest_blockhash_with_commitment(CommitmentConfig {
            commitment: CommitmentLevel::Confirmed,
        })
        .await?
        .0;

    let mut highest_slot = 0;
    let mut is_leader_slot = false;

    let mut tick = interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                maintenance_tick(&mut searcher_client, &rpc_client, &mut leader_schedule, &mut blockhash, regions.clone()).await?;
            }
            maybe_bundle_result = bundle_results_receiver.recv() => {
                let bundle_result: BundleResult = maybe_bundle_result.ok_or(BackrunError::Shutdown)?;
                info!("received bundle_result: [bundle_id={:?}, result={:?}]", bundle_result.bundle_id, bundle_result.result);
            }
            maybe_pending_tx_notification = pending_tx_receiver.recv() => {
                // block engine starts forwarding a few slots early, for super high activity accounts
                // it might be ideal to wait until the leader slot is up
                if is_leader_slot {
                    let pending_tx_notification = maybe_pending_tx_notification.ok_or(BackrunError::Shutdown)?;
                    let bundles = build_bundles(pending_tx_notification, keypair, &blockhash, &tip_accounts, &mut rng, &message);
                    if !bundles.is_empty() {
                        let now = Instant::now();
                        let results = send_bundles(&mut searcher_client, &bundles).await?;
                        let send_elapsed = now.elapsed().as_micros() as u64;
                        let send_rt_pp_us = send_elapsed / bundles.len() as u64;

                        match block_stats.entry(highest_slot) {
                            Entry::Occupied(mut entry) => {
                                let stats = entry.get_mut();
                                stats.bundles_sent.extend(bundles.into_iter().zip(results.into_iter()));
                                stats.send_elapsed += send_elapsed;
                                let _ = stats.send_rt_per_packet.increment(send_rt_pp_us);
                            }
                            Entry::Vacant(entry) => {
                                let mut send_rt_per_packet = Histogram::new();
                                let _ = send_rt_per_packet.increment(send_rt_pp_us);
                                entry.insert(BlockStats {
                                    bundles_sent: bundles.into_iter().zip(results.into_iter()).collect(),
                                    send_elapsed,
                                    send_rt_per_packet
                                });
                            }
                        }
                    }
                }
            }
            maybe_slot = slot_receiver.recv() => {
                highest_slot = maybe_slot.ok_or(BackrunError::Shutdown)?;
                is_leader_slot = leader_schedule.iter().any(|(_, slots)| slots.contains(&highest_slot));
            }
            maybe_block = block_receiver.recv() => {
                let block = maybe_block.ok_or(BackrunError::Shutdown)?;
                print_block_stats(&mut block_stats, block, &leader_schedule, &mut block_signatures);
            }
        }
    }
}

fn main() -> Result<()> {
    env_logger::builder()
        .format_timestamp(Some(TimestampPrecision::Micros))
        .init();
    let args: Args = Args::parse();

    let payer_keypair = Arc::new(read_keypair_file(&args.payer_keypair).expect("parse kp file"));
    let auth_keypair = args
        .auth_keypair
        .as_ref()
        .map(|path| Arc::new(read_keypair_file(path).expect("parse kp file")));

    set_host_id(
        auth_keypair
            .as_ref()
            .map(|kp| kp.pubkey().to_string())
            .unwrap_or(uuid::Uuid::new_v4().to_string()),
    );

    let runtime = Builder::new_multi_thread().enable_all().build().unwrap();

    match auth_keypair {
        Some(auth_keypair) => {
            let searcher_client_auth = runtime.block_on(
            get_searcher_client_auth(
                args.block_engine_url.as_str(),
                &auth_keypair,
            ))
            .expect("Failed to get searcher client with auth. Note: If you don't pass in the auth keypair, we can attempt to connect to the no auth endpoint");
            start_searcher_loop(runtime, searcher_client_auth, &payer_keypair, args)
        }
        None => {
            let searcher_client_no_auth = runtime.block_on(
                get_searcher_client_no_auth(
                    args.block_engine_url.as_str(),
                ))
                .expect("Failed to get searcher client with auth. Note: If you don't pass in the auth keypair, we can attempt to connect to the no auth endpoint");
            start_searcher_loop(runtime, searcher_client_no_auth, &payer_keypair, args)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn start_searcher_loop<T>(
    runtime: tokio::runtime::Runtime,
    searcher_client: SearcherServiceClient<T>,
    payer_keypair: &Keypair,
    args: Args,
) -> Result<()>
where
    T: tonic::client::GrpcService<tonic::body::BoxBody> + Send + 'static + Clone,
    T::Error: Into<StdError>,
    T::ResponseBody: Body<Data = Bytes> + Send + 'static,
    <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    <T as tonic::client::GrpcService<tonic::body::BoxBody>>::Future: std::marker::Send,
{
    runtime.block_on(async move {
        let (slot_sender, slot_receiver) = channel(100);
        let (block_sender, block_receiver) = channel(100);
        let (bundle_results_sender, bundle_results_receiver) = channel(100);
        let (pending_tx_sender, pending_tx_receiver) = channel(100);

        tokio::spawn(slot_subscribe_loop(args.pubsub_url.clone(), slot_sender));
        tokio::spawn(block_subscribe_loop(args.pubsub_url.clone(), block_sender));
        tokio::spawn(pending_tx_loop(
            searcher_client.clone(),
            pending_tx_sender,
            args.backrun_accounts,
        ));

        if args.subscribe_bundle_results {
            tokio::spawn(bundle_results_loop(
                searcher_client.clone(),
                bundle_results_sender,
            ));
        }

        let result = run_searcher_loop(
            searcher_client,
            payer_keypair,
            args.rpc_url,
            args.regions,
            args.message,
            args.tip_program_id,
            slot_receiver,
            block_receiver,
            bundle_results_receiver,
            pending_tx_receiver,
        )
        .await;
        error!("searcher loop exited result: {result:?}");

        Ok(())
    })
}
