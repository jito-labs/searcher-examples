mod event_loops;

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;
use std::{path::Path, result, str::FromStr, sync::Arc, time::Duration};

use crate::event_loops::{block_subscribe_loop, pending_tx_loop, slot_subscribe_loop};
use clap::Parser;
use env_logger::TimestampPrecision;
use histogram::Histogram;
use jito_protos::bundle::Bundle;
use jito_protos::convert::{proto_packet_from_versioned_tx, versioned_tx_from_packet};
use jito_protos::searcher::{
    searcher_service_client::SearcherServiceClient, ConnectedLeadersRequest,
    NextScheduledLeaderRequest, PendingTxNotification, SendBundleRequest, SendBundleResponse,
};
use log::*;
use rand::rngs::ThreadRng;
use rand::{thread_rng, Rng};
use searcher_service_client::token_authenticator::ClientInterceptor;
use searcher_service_client::{get_searcher_client, BlockEngineConnectionError};
use solana_client::client_error::ClientError;
use solana_client::nonblocking::pubsub_client::PubsubClientError;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_response;
use solana_client::rpc_response::RpcBlockUpdate;
use solana_metrics::{datapoint_info, set_host_id};
use solana_sdk::clock::Slot;
use solana_sdk::commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_sdk::hash::Hash;
use solana_sdk::signature::{Signature, Signer};
use solana_sdk::system_instruction::transfer;
use solana_sdk::transaction::Transaction;
use solana_sdk::transaction::VersionedTransaction;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair},
};
use spl_memo::build_memo;
use thiserror::Error;
use tokio::sync::mpsc::{channel, Receiver};
use tokio::{runtime::Builder, time::interval};
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;
use tonic::{Response, Status};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Address for auth service
    #[clap(long, env)]
    auth_addr: String,

    /// Address for searcher service
    #[clap(long, env)]
    searcher_addr: String,

    /// Accounts to backrun
    #[clap(long, env)]
    backrun_accounts: Vec<String>,

    /// Path to keypair file used to sign and pay for transactions
    #[clap(long, env)]
    payer_keypair: String,

    /// Path to keypair file used to authenticate with the backend
    #[clap(long, env)]
    auth_keypair: String,

    /// Pubsub URL. Note that this RPC server must have --rpc-pubsub-enable-block-subscription enabled
    #[clap(long, env)]
    pubsub_url: String,

    /// RPC URL to get block hashes from
    #[clap(long, env)]
    rpc_url: String,

    /// Memo program message
    #[clap(long, env, default_value_t = String::from("jito backrun"))]
    message: String,

    /// Tip program public key
    #[clap(long, env)]
    tip_program_id: String,
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

#[derive(Default, Clone)]
struct BlockStats {
    bundles_sent: Vec<(
        BundledTransactions,
        Rc<result::Result<Response<SendBundleResponse>, Status>>,
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
                    transfer(&keypair.pubkey(), &tip_account, 1),
                ],
                Some(&keypair.pubkey()),
                &[keypair],
                blockhash.clone(),
            ));
            Some(BundledTransactions {
                mempool_txs: vec![mempool_tx],
                backrun_txs: vec![backrun_tx],
            })
        })
        .collect()
}

async fn send_bundles(
    searcher_client: &mut SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>,
    bundles: &[BundledTransactions],
) -> Result<Vec<result::Result<Response<SendBundleResponse>, Status>>> {
    let mut futs = vec![];
    for b in bundles {
        let mut searcher_client = searcher_client.clone();
        let packets = b
            .mempool_txs
            .iter()
            .map(proto_packet_from_versioned_tx)
            .chain(b.backrun_txs.iter().map(proto_packet_from_versioned_tx))
            .collect();

        let task = tokio::spawn(async move {
            searcher_client
                .send_bundle(SendBundleRequest {
                    bundle: Some(Bundle {
                        header: None,
                        packets,
                    }),
                })
                .await
        });
        futs.push(task);
    }

    let responses = futures::future::join_all(futs).await;
    let send_bundle_responses = responses.into_iter().map(|r| r.unwrap()).collect();
    Ok(send_bundle_responses)
}

fn generate_tip_accounts(tip_program_pubkey: &Pubkey) -> Vec<Pubkey> {
    let tip_pda_0 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_0"], &tip_program_pubkey).0;
    let tip_pda_1 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_1"], &tip_program_pubkey).0;
    let tip_pda_2 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_2"], &tip_program_pubkey).0;
    let tip_pda_3 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_3"], &tip_program_pubkey).0;
    let tip_pda_4 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_4"], &tip_program_pubkey).0;
    let tip_pda_5 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_5"], &tip_program_pubkey).0;
    let tip_pda_6 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_6"], &tip_program_pubkey).0;
    let tip_pda_7 = Pubkey::find_program_address(&[b"TIP_ACCOUNT_7"], &tip_program_pubkey).0;

    vec![
        tip_pda_0, tip_pda_1, tip_pda_2, tip_pda_3, tip_pda_4, tip_pda_5, tip_pda_6, tip_pda_7,
    ]
}

async fn maintenance_tick(
    searcher_client: &mut SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>,
    rpc_client: &RpcClient,
    leader_schedule: &mut HashMap<Pubkey, HashSet<Slot>>,
    blockhash: &mut Hash,
) -> Result<()> {
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
        .get_next_scheduled_leader(NextScheduledLeaderRequest {})
        .await?
        .into_inner();
    info!(
        "next_scheduled_leader: {} in {} slots",
        next_scheduled_leader.next_leader_identity,
        next_scheduled_leader.next_leader_slot - next_scheduled_leader.current_slot
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
                "sent_rt_pp_min",
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
                .map(|s| Signature::from_str(&s).unwrap())
                .collect();

            // bundles that were sent before or during this slot
            let bundles_sent_before_slot: HashMap<Slot, BlockStats> = block_stats
                .iter()
                .filter(|(slot, _)| **slot <= block.context.slot)
                .map(|(slot, stats)| (*slot, stats.clone()))
                .collect();

            if let Some(leader) = maybe_leader {
                // number of bundles sent before or during this slot
                let num_bundles_sent: usize = bundles_sent_before_slot
                    .iter()
                    .map(|(_, stats)| stats.bundles_sent.len())
                    .sum();

                // number of bundles where sending returned ok
                let num_bundles_sent_ok: usize = bundles_sent_before_slot
                    .iter()
                    .map(|(_, stats)| {
                        stats
                            .bundles_sent
                            .iter()
                            .filter(|(_, send_response)| send_response.is_ok())
                            .count()
                    })
                    .sum();

                // a list of all bundles landed this slot that were sent before or during this slot
                let bundles_landed: Vec<(Slot, &BundledTransactions, usize)> =
                    bundles_sent_before_slot
                        .iter()
                        .flat_map(|(slot, stats)| {
                            stats
                                .bundles_sent
                                .iter()
                                .enumerate()
                                .filter(|(index, (_, send_response))| send_response.is_ok())
                                .filter_map(|(index, (bundle_sent, _))| {
                                    if bundle_sent
                                        .backrun_txs
                                        .iter()
                                        .chain(bundle_sent.mempool_txs.iter())
                                        .all(|tx| block_signatures.contains(&tx.signatures[0]))
                                    {
                                        Some((*slot, bundle_sent, index))
                                    } else {
                                        None
                                    }
                                })
                        })
                        .collect();

                // let mut bundles_not_processed: HashMap<Slot, BlockStats> = bundles_sent_before_slot
                //     .iter()
                //     .map(|(slot, stats)| {
                //         let filt_stats = BlockStats {
                //             bundles_sent: stats
                //                 .bundles_sent
                //                 .iter()
                //                 .enumerate()
                //                 .filter_map(|(index, (bund, res))| {
                //                     if bundles_landed.contains(&(*slot, *bund, index)) {}
                //                     Some((bund.clone(), res.clone()))
                //                 })
                //                 .collect(),
                //             send_elapsed: stats.send_elapsed,
                //             send_rt_per_packet: stats.send_rt_per_packet.clone(),
                //         };
                //         (*slot, filt_stats)
                //     })
                //     .collect();

                for bundle in bundles_landed.iter() {
                    bundles_sent_before_slot.entry(bundle.0).and_modify(|b| {
                        b.bundles_sent.remove(bundle.2);
                    });
                }

                let mempool_txs_landed_no_bundle: Vec<(Slot, &BundledTransactions)> =
                    bundles_sent_before_slot
                        .iter()
                        .flat_map(|(slot, stats)| {
                            stats
                                .bundles_sent
                                .iter()
                                .enumerate()
                                .filter(|(index, (_, send_response))| send_response.is_ok())
                                .filter_map(|(index, (bundle_sent, _))| {
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
                    .map(|(slot, _, _)| *slot)
                    .min()
                    .unwrap_or(0);
                let max_bundle_send_slot = bundles_landed
                    .iter()
                    .map(|(slot, _, _)| *slot)
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

                // Clear out all stats before this slot
                // ToDo (JL): This may be unnecessary since block stats are moved out above
                block_stats.retain(|slot, _| *slot > block.context.slot);
                // Add back any bundles that didn't land or have their mempool tx land
                block_stats.extend(bundles_sent_before_slot)
            } else {
                // figure out how many transactions in bundles landed in slots other than our leader
                let num_mempool_txs_landed: usize = bundles_sent_before_slot
                    .iter()
                    .map(|(_, stats)| {
                        stats
                            .bundles_sent
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

async fn run_searcher_loop(
    auth_addr: String,
    searcher_addr: String,
    auth_keypair: Arc<Keypair>,
    keypair: &Keypair,
    rpc_url: String,
    message: String,
    tip_program_pubkey: Pubkey,
    mut slot_receiver: Receiver<Slot>,
    mut block_receiver: Receiver<rpc_response::Response<RpcBlockUpdate>>,
    mut pending_tx_receiver: Receiver<PendingTxNotification>,
) -> Result<()> {
    let mut leader_schedule: HashMap<Pubkey, HashSet<Slot>> = HashMap::new();
    let mut block_stats: HashMap<Slot, BlockStats> = HashMap::new();
    let mut block_signatures: HashMap<Slot, HashSet<Signature>> = HashMap::new();

    let mut searcher_client =
        get_searcher_client(&auth_addr, &searcher_addr, &auth_keypair).await?;

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
                maintenance_tick(&mut searcher_client, &rpc_client, &mut leader_schedule, &mut blockhash).await?;
            }
            maybe_pending_tx_notification = pending_tx_receiver.recv() => {
                // block engine starts forwarding a few slots early, for super high activity accounts
                // it might be ideal to wait until the leader slot is up
                if is_leader_slot {
                    let pending_tx_notification = maybe_pending_tx_notification.ok_or(BackrunError::Shutdown)?;
                    let bundles = build_bundles(pending_tx_notification, &keypair, &blockhash, &tip_accounts, &mut rng, &message);
                    if !bundles.is_empty() {
                        let now = Instant::now();
                        let results = send_bundles(&mut searcher_client, &bundles).await?;
                        let send_elapsed = now.elapsed().as_micros() as u64;
                        let send_rt_pp_us = send_elapsed / bundles.len() as u64;

                        match block_stats.entry(highest_slot) {
                            Entry::Occupied(mut entry) => {
                                let mut stats = entry.get_mut();
                                stats.bundles_sent.extend(bundles.into_iter().zip(results.into_iter().map(|r| Rc::new(r))));
                                stats.send_elapsed += send_elapsed;
                                let _ = stats.send_rt_per_packet.increment(send_rt_pp_us);
                            }
                            Entry::Vacant(entry) => {
                                let mut send_rt_per_packet = Histogram::new();
                                let _ = send_rt_per_packet.increment(send_rt_pp_us);
                                entry.insert(BlockStats {
                                    bundles_sent: bundles.into_iter().zip(results.into_iter().map(|r| Rc::new(r))).collect(),
                                    send_elapsed: send_elapsed,
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

    let payer_keypair =
        Arc::new(read_keypair_file(Path::new(&args.payer_keypair)).expect("parse kp file"));
    let auth_keypair =
        Arc::new(read_keypair_file(Path::new(&args.auth_keypair)).expect("parse kp file"));

    set_host_id(auth_keypair.pubkey().to_string());

    let backrun_pubkeys: Vec<Pubkey> = args
        .backrun_accounts
        .iter()
        .map(|a| Pubkey::from_str(a).unwrap())
        .collect();
    let tip_program_pubkey = Pubkey::from_str(&args.tip_program_id).unwrap();

    let runtime = Builder::new_multi_thread().enable_all().build().unwrap();
    runtime.block_on(async move {
        let (slot_sender, slot_receiver) = channel(100);
        let (block_sender, block_receiver) = channel(100);
        let (pending_tx_sender, pending_tx_receiver) = channel(100);

        tokio::spawn(slot_subscribe_loop(args.pubsub_url.clone(), slot_sender));
        tokio::spawn(block_subscribe_loop(args.pubsub_url.clone(), block_sender));
        tokio::spawn(pending_tx_loop(
            args.auth_addr.clone(),
            args.searcher_addr.clone(),
            auth_keypair.clone(),
            pending_tx_sender,
            backrun_pubkeys,
        ));

        let result = run_searcher_loop(
            args.auth_addr,
            args.searcher_addr,
            auth_keypair,
            &payer_keypair,
            args.rpc_url,
            args.message,
            tip_program_pubkey,
            slot_receiver,
            block_receiver,
            pending_tx_receiver,
        )
        .await;
        error!("searcher loop exited result: {:?}", result);

        Ok(())
    })
}
