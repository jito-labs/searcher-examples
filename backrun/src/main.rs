use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::{path::Path, result, str::FromStr, sync::Arc, time::Duration};

use clap::Parser;
use env_logger::TimestampPrecision;
use futures_util::StreamExt;
use jito_protos::auth::auth_service_client::AuthServiceClient;
use jito_protos::auth::Role;
use jito_protos::bundle::Bundle;
use jito_protos::convert::{proto_packet_from_versioned_tx, versioned_tx_from_packet};
use jito_protos::searcher::{
    searcher_service_client::SearcherServiceClient, ConnectedLeadersRequest,
    NextScheduledLeaderRequest, PendingTxNotification, PendingTxSubscriptionRequest,
    SendBundleRequest, SendBundleResponse,
};
use log::*;
use rand::rngs::ThreadRng;
use rand::{thread_rng, Rng};
use searcher_service_client::token_authenticator::ClientInterceptor;
use solana_client::client_error::ClientError;
use solana_client::nonblocking::pubsub_client::{PubsubClient, PubsubClientError};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcBlockSubscribeConfig, RpcBlockSubscribeFilter};
use solana_client::rpc_response;
use solana_client::rpc_response::{RpcBlockUpdate, SlotUpdate};
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
use solana_transaction_status::{TransactionDetails, UiTransactionEncoding};
use spl_memo::build_memo;
use thiserror::Error;
use tokio::{runtime::Builder, time::interval};
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;
use tonic::{transport::Endpoint, Response, Status};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Address for auth service
    #[clap(long, env)]
    auth_addr: String,

    /// Address for searcher service
    #[clap(long, env)]
    searcher_addr: String,

    /// Account to backrun
    #[clap(long, env)]
    backrun_account: String,

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
    #[error("TonicError")]
    TonicError(#[from] tonic::transport::Error),
    #[error("GrpcError")]
    GrpcError(#[from] Status),
    #[error("RpcError")]
    RpcError(#[from] ClientError),
    #[error("PubSubError")]
    PubSubError(#[from] PubsubClientError),
}

#[derive(Clone)]
struct BundledTransactions {
    mempool_txs: Vec<VersionedTransaction>,
    backrun_txs: Vec<VersionedTransaction>,
}

struct BlockStats {
    bundles_sent: Vec<(
        BundledTransactions,
        result::Result<Response<SendBundleResponse>, Status>,
    )>,
}

type Result<T> = result::Result<T, BackrunError>;

async fn create_grpc_channel(url: &str) -> Result<Channel> {
    let mut endpoint = Endpoint::from_shared(url.to_string()).expect("invalid url");
    if url.contains("https") {
        endpoint = endpoint.tls_config(tonic::transport::ClientTlsConfig::new())?;
    }
    Ok(endpoint.connect().await?)
}

fn build_bundles(
    maybe_pending_tx_notification: Option<result::Result<PendingTxNotification, Status>>,
    keypair: &Keypair,
    blockhash: &Hash,
    tip_accounts: &[Pubkey],
    rng: &mut ThreadRng,
    message: &str,
) -> Result<Vec<BundledTransactions>> {
    match maybe_pending_tx_notification {
        Some(Ok(pending_txs)) => {
            let bundles: Vec<BundledTransactions> = pending_txs
                .transactions
                .into_iter()
                .filter_map(|packet| {
                    let mempool_tx = versioned_tx_from_packet(&packet)?;
                    let tip_account = tip_accounts[rng.gen_range(0..tip_accounts.len())];

                    let backrun_tx =
                        VersionedTransaction::from(Transaction::new_signed_with_payer(
                            &[
                                build_memo(
                                    format!(
                                        "{}: {:?}",
                                        message,
                                        mempool_tx.signatures[0].to_string()
                                    )
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
                .collect();

            Ok(bundles)
        }
        Some(Err(e)) => {
            return Err(e.into());
        }
        None => {
            return Err(Status::unavailable("unavailable").into());
        }
    }
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
    block: Option<rpc_response::Response<RpcBlockUpdate>>,
    leader_schedule: &HashMap<Pubkey, HashSet<Slot>>,
    block_signatures: &mut HashMap<Slot, HashSet<Signature>>,
) -> Result<()> {
    const KEEP_SIGS_SLOTS: u64 = 20;

    let block = block
        .ok_or_else(|| PubsubClientError::ConnectionClosed("block_subscribe closed".to_string()))?;

    datapoint_info!("block-received", ("slot", block.context.slot, i64));

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
            let bundles_sent_before_slot: HashMap<
                &Slot,
                &Vec<(
                    BundledTransactions,
                    result::Result<Response<SendBundleResponse>, Status>,
                )>,
            > = block_stats
                .iter()
                .filter(|(slot, _)| **slot <= block.context.slot)
                .map(|(slot, stats)| (slot, stats.bundles_sent.as_ref()))
                .collect();

            if let Some(leader) = maybe_leader {
                // number of bundles sent before or during this slot
                let num_bundles_sent: usize = bundles_sent_before_slot
                    .iter()
                    .map(|(_, bundles_sent)| bundles_sent.len())
                    .sum();

                // number of bundles where sending returned ok
                let num_bundles_sent_ok: usize = bundles_sent_before_slot
                    .iter()
                    .map(|(_, bundles_sent)| {
                        bundles_sent
                            .iter()
                            .filter(|(_, send_response)| send_response.is_ok())
                            .count()
                    })
                    .sum();

                // a list of all bundles landed this slot that were sent before or during this slot
                let bundles_landed: Vec<(&Slot, &BundledTransactions)> = bundles_sent_before_slot
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

                // find the min and max distance from when the bundle was sent to what block it landed in
                let min_bundle_send_slot = bundles_landed
                    .iter()
                    .map(|(slot, _)| **slot)
                    .min()
                    .unwrap_or(0);
                let max_bundle_send_slot = bundles_landed
                    .iter()
                    .map(|(slot, _)| **slot)
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
                    .iter()
                    .map(|(_, bundles)| {
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

    Ok(())
}

async fn run_searcher_loop(
    mut searcher_client: SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>,
    backrun_pubkey: Pubkey,
    keypair: &Keypair,
    pubsub_url: String,
    rpc_url: String,
    message: String,
    tip_program_pubkey: Pubkey,
) -> Result<()> {
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

    let pending_tx_stream_response = searcher_client
        .subscribe_pending_transactions(PendingTxSubscriptionRequest {
            accounts: vec![backrun_pubkey.to_string()],
        })
        .await?;
    let mut pending_tx_stream = pending_tx_stream_response.into_inner();

    let pubsub_client = PubsubClient::new(&pubsub_url).await?;
    let (mut slot_update_subscription, _unsubscribe_fn) =
        pubsub_client.slot_updates_subscribe().await?;

    let (mut block_update_subscription, _unsubscribe_fn) = pubsub_client
        .block_subscribe(
            RpcBlockSubscribeFilter::All,
            Some(RpcBlockSubscribeConfig {
                commitment: Some(CommitmentConfig {
                    commitment: CommitmentLevel::Confirmed,
                }),
                encoding: Some(UiTransactionEncoding::Base64),
                transaction_details: Some(TransactionDetails::Signatures),
                show_rewards: Some(true),
                max_supported_transaction_version: None,
            }),
        )
        .await?;

    let mut highest_slot = 0;

    let mut tick = interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                maintenance_tick(&mut searcher_client, &rpc_client, &mut leader_schedule, &mut blockhash).await?;
            }
            maybe_pending_tx_notification = pending_tx_stream.next() => {
                let bundles = build_bundles(maybe_pending_tx_notification, &keypair, &blockhash, &tip_accounts, &mut rng, &message)?;
                if !bundles.is_empty() {
                    let results = send_bundles(&mut searcher_client, &bundles).await?;
                    debug!("sent bundles num: {:?}", bundles.len());

                    let mut inserted_new = false;
                    match block_stats.entry(highest_slot) {
                        Entry::Occupied(mut entry) => {
                            entry.get_mut().bundles_sent.extend(bundles.into_iter().zip(results.into_iter()))
                        }
                        Entry::Vacant(entry) => {
                            inserted_new = true;
                            entry.insert(BlockStats {
                                bundles_sent: bundles.into_iter().zip(results.into_iter()).collect(),
                            });
                        }
                    }
                    if inserted_new {
                        if let Some(stats) = block_stats.get(&(highest_slot - 1)) {
                            datapoint_info!("bundles-sent", ("slot", highest_slot - 1, i64), ("bundles", stats.bundles_sent.len(), i64));
                        }
                    }
                }
            }
            maybe_slot_update = slot_update_subscription.next() => {
                match maybe_slot_update {
                    Some(SlotUpdate::FirstShredReceived {slot, timestamp: _}) => {
                        datapoint_info!("slot-first-shred-received", ("slot", slot , i64));
                        highest_slot = slot;
                    }
                    Some(_) => {}
                    None => {
                        return Err(PubsubClientError::ConnectionClosed("slot update connection closed".to_string()).into());
                    }
                }
            }
            maybe_block = block_update_subscription.next() => {
                print_block_stats(&mut block_stats, maybe_block, &leader_schedule, &mut block_signatures)?;
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
        Arc::new(read_keypair_file(Path::new(&args.payer_keypair)).expect("parse kp file"));

    set_host_id(auth_keypair.pubkey().to_string());

    let backrun_pubkey = Pubkey::from_str(&args.backrun_account).unwrap();
    let tip_program_pubkey = Pubkey::from_str(&args.tip_program_id).unwrap();

    let runtime = Builder::new_multi_thread().enable_all().build().unwrap();
    runtime.block_on(async move {
        let auth_channel = create_grpc_channel(&args.auth_addr).await?;
        let client_interceptor = ClientInterceptor::new(
            AuthServiceClient::new(auth_channel),
            &auth_keypair,
            Role::Searcher,
        )
        .await?;
        info!(
            "authenticated against block engine url: {:?}",
            args.auth_addr
        );

        let searcher_channel = create_grpc_channel(&args.searcher_addr).await?;
        let searcher_client =
            SearcherServiceClient::with_interceptor(searcher_channel, client_interceptor);
        info!(
            "connected to searcher service url: {:?}",
            args.searcher_addr
        );

        let result = run_searcher_loop(
            searcher_client,
            backrun_pubkey,
            &payer_keypair,
            args.pubsub_url,
            args.rpc_url,
            args.message,
            tip_program_pubkey,
        )
        .await;
        error!("searcher loop exited result: {:?}", result);

        Ok(())
    })
}
