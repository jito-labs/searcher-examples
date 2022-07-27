use std::collections::HashSet;
use std::{
    collections::VecDeque,
    path::Path,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use futures_util::StreamExt;
use jito_protos::searcher::ConnectedLeadersRequest;
use jito_protos::searcher::NextScheduledLeaderRequest;
use jito_protos::{
    bundle::Bundle,
    convert::{proto_packet_from_versioned_tx, versioned_tx_from_packet},
    searcher::{
        searcher_service_client::SearcherServiceClient, PendingTxNotification,
        PendingTxSubscriptionRequest, SendBundleRequest,
    },
    shared::Header,
};
use log::*;
use prost_types::Timestamp;
use solana_client::{
    nonblocking::pubsub_client::PubsubClient,
    rpc_config::{RpcBlockSubscribeConfig, RpcBlockSubscribeFilter},
    rpc_response::{Response, RpcBlockUpdate},
};
use solana_sdk::{
    clock::Slot,
    commitment_config::{CommitmentConfig, CommitmentLevel},
    hash::Hash,
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair, Signer},
    transaction::{Transaction, VersionedTransaction},
};
use solana_transaction_status::{TransactionDetails, UiTransactionEncoding};
use spl_memo::build_memo;
use tokio::runtime::Builder;
use tokio::time::interval;
use tonic::{transport::Channel, Status};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Address for searcher service
    #[clap(long, env)]
    searcher_addr: String,

    /// Backrun account defaults to pyth oracle on testnet
    #[clap(long, env, default_value_t = String::from("8tfDNiaEyrV6Q1U4DEXrEigs9DoDtkugzFbybENEbCDz"))]
    backrun_account: String,

    /// Path to Keypair file
    #[clap(long, env)]
    keypair_file: String,

    /// Pubsub URL
    #[clap(long, env)]
    pubsub_url: String,
}

#[derive(Debug)]
#[allow(dead_code)]
struct BackrunResponse {
    bundle: SendBundleRequest,
    tx: VersionedTransaction,
    backrun_tx: VersionedTransaction,
    uuid: String,
    time_sent: Instant,
}

#[derive(Debug)]
#[allow(dead_code)] // just for debug
struct BackrunStats {
    slot: Slot,
    num_expired: usize,
    num_tx_beat_ours: usize,
    num_successful_bundles: usize,
    num_pending: usize,
}

fn main() {
    env_logger::init();

    let args: Args = Args::parse();

    let kp = Arc::new(read_keypair_file(Path::new(&args.keypair_file)).expect("parse kp file"));
    let pubkey = Pubkey::from_str(&args.backrun_account).unwrap();
    let searcher_url = args.searcher_addr;
    let pubsub_url = args.pubsub_url;

    let runtime = Builder::new_multi_thread().enable_all().build().unwrap();
    runtime.block_on(async move {
        let mut searcher_client = SearcherServiceClient::connect(searcher_url)
            .await
            .expect("searcher service can't connect");
        info!("connected to searcher");

        let pubsub_client = PubsubClient::new(&pubsub_url).await.expect("subscribing to pubsub client");
        let (mut block_notifications, _block_unsubscribe) = pubsub_client.block_subscribe(
            RpcBlockSubscribeFilter::All,
            Some(RpcBlockSubscribeConfig {
                commitment: Some(CommitmentConfig { commitment: CommitmentLevel::Confirmed }),
                encoding: Some(UiTransactionEncoding::Base64),
                transaction_details: Some(TransactionDetails::Signatures),
                show_rewards: Some(false),
                max_supported_transaction_version: Some(0),
            })).await.expect("block subscribing");
        let response = searcher_client
            .subscribe_pending_transactions(PendingTxSubscriptionRequest {
                accounts: vec![pubkey.to_string()],
            })
            .await
            .expect("subscribe to pending transactions");
        info!("subscribed to pending transactions");

        let mut valid_blockhashes: VecDeque<Hash> = VecDeque::new();

        let mut connected_leader_slots: HashSet<Slot> = HashSet::new();

        let mut backruns = Vec::new();
        let mut tick = interval(Duration::from_secs(5));

        let mut pending_tx_stream = response.into_inner();
        loop {
            tokio::select! {
                maybe_pending_tx_notification = pending_tx_stream.message() => {
                    let transactions_of_interest = get_transactions_of_interest(maybe_pending_tx_notification, &pubkey, &valid_blockhashes).expect("gets transactions");
                    let new_backruns = backrun_transaction(transactions_of_interest, &mut searcher_client, &valid_blockhashes, &kp).await.expect("sends bundles");
                    backruns.extend(new_backruns);
                }
                maybe_block = block_notifications.next() => {
                    let backrun_stats = update_block_stats(maybe_block, &mut valid_blockhashes, &mut backruns);
                    if connected_leader_slots.contains(&backrun_stats.slot) {
                        info!("backrun_stats: {:?}", backrun_stats);
                    }
                }
                _ = tick.tick() => {
                    let next_leader_info = searcher_client.get_next_scheduled_leader(NextScheduledLeaderRequest{}).await.expect("gets next slot").into_inner();
                    let slots_until_next = next_leader_info.next_leader_slot - next_leader_info.current_slot;
                    info!("next leader slot in {:?} slots, pubkey: {:?}", slots_until_next, next_leader_info.next_leader_identity);

                    let leader_schedule = searcher_client.get_connected_leaders(ConnectedLeadersRequest{}).await.expect("get connected leaders").into_inner();
                    connected_leader_slots = leader_schedule.connected_validators.values().fold(HashSet::new(), |mut set, slot_list| {
                        set.extend(slot_list.slots.clone());
                        set
                    });
                }
            }
        }
    });
}

fn update_block_stats(
    maybe_block: Option<Response<RpcBlockUpdate>>,
    valid_blockhashes: &mut VecDeque<Hash>,
    backruns: &mut Vec<BackrunResponse>,
) -> BackrunStats {
    let mut tx_beat_ours = 0;
    let mut successful_bundles = 0;
    let mut slot = 0;
    if let Some(response) = maybe_block {
        slot = response.context.slot;

        if let Some(block) = response.value.block {
            valid_blockhashes.push_front(Hash::from_str(&block.blockhash).unwrap());
            if valid_blockhashes.len() > 100 {
                valid_blockhashes.pop_back();
            }

            if let Some(signatures) = block.signatures {
                for backrun in backruns.iter() {
                    let is_tx_present = signatures.contains(&backrun.tx.signatures[0].to_string());
                    let is_backrun_present =
                        signatures.contains(&backrun.backrun_tx.signatures[0].to_string());
                    match (is_backrun_present, is_tx_present) {
                        (false, true) => {
                            tx_beat_ours += 1;
                        }
                        (true, true) => {
                            successful_bundles += 1;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    let len_before = backruns.len();
    backruns.retain(|b| b.time_sent.elapsed() < Duration::from_secs(30));
    let len_after = backruns.len();

    BackrunStats {
        slot,
        num_expired: len_before - len_after,
        num_tx_beat_ours: tx_beat_ours,
        num_successful_bundles: successful_bundles,
        num_pending: len_after,
    }
}

async fn backrun_transaction(
    transactions_of_interest: Vec<VersionedTransaction>,
    searcher_client: &mut SearcherServiceClient<Channel>,
    valid_blockhashes: &VecDeque<Hash>,
    kp: &Arc<Keypair>,
) -> Result<Vec<BackrunResponse>, Status> {
    let blockhash = valid_blockhashes.front().unwrap();

    let tasks = transactions_of_interest.into_iter().map(|tx| {
        let mut searcher_client = searcher_client.clone();
        let kp = kp.clone();
        let blockhash = *blockhash;

        tokio::spawn(async move {
            let time_sent = Instant::now();
            let backrun_tx = VersionedTransaction::from(Transaction::new_signed_with_payer(
                &[build_memo(
                    format!("jito backrun: {:?}", tx.signatures[0].to_string()).as_bytes(),
                    &[],
                )],
                Some(&kp.pubkey()),
                &[kp.as_ref()],
                blockhash,
            ));
            let bundle = SendBundleRequest {
                bundle: Some(Bundle {
                    header: Some(Header {
                        ts: Some(Timestamp::from(SystemTime::now())),
                    }),
                    packets: vec![
                        proto_packet_from_versioned_tx(&tx),
                        proto_packet_from_versioned_tx(&backrun_tx),
                    ],
                }),
            };

            if let Ok(response) = searcher_client.send_bundle(bundle.clone()).await {
                Some(BackrunResponse {
                    bundle,
                    tx: tx.clone(),
                    backrun_tx,
                    uuid: response.into_inner().uuid,
                    time_sent,
                })
            } else {
                None
            }
        })
    });
    let results = futures::future::join_all(tasks).await;

    Ok(results
        .into_iter()
        .filter(|r| r.is_ok())
        .filter(|r| r.as_ref().unwrap().is_some())
        .map(|r| r.unwrap().unwrap())
        .collect())
}

fn get_transactions_of_interest(
    maybe_pending_tx_notification: Result<Option<PendingTxNotification>, Status>,
    pubkey: &Pubkey,
    valid_blockhashes: &VecDeque<Hash>,
) -> Result<Vec<VersionedTransaction>, Status> {
    return match maybe_pending_tx_notification {
        Ok(Some(pending_tx_notification)) => {
            let expiration_time_since_epoch = pending_tx_notification
                .expiration_time
                .as_ref()
                .map(|ts| SystemTime::try_from(ts.clone()).unwrap())
                .unwrap_or(SystemTime::UNIX_EPOCH)
                .duration_since(UNIX_EPOCH)
                .unwrap();
            let server_time_since_epoch = pending_tx_notification
                .server_side_ts
                .as_ref()
                .map(|ts| SystemTime::try_from(ts.clone()).unwrap())
                .unwrap_or(SystemTime::UNIX_EPOCH)
                .duration_since(UNIX_EPOCH)
                .unwrap();

            let transactions: Vec<VersionedTransaction> = pending_tx_notification
                .transactions
                .iter()
                .filter_map(versioned_tx_from_packet)
                .filter(|tx| tx.message.static_account_keys().contains(pubkey))
                .filter(|tx| valid_blockhashes.contains(tx.message.recent_blockhash()))
                .collect();

            if !transactions.is_empty() {
                let now_since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
                let server_latency_us = now_since_epoch.as_micros() as i128
                    - server_time_since_epoch.as_micros() as i128;
                let time_to_expiration_us = expiration_time_since_epoch.as_micros() as i128
                    - now_since_epoch.as_micros() as i128;

                info!("time_to_expiration_us: {:?}", time_to_expiration_us);
                info!("server_latency_us {:?}", server_latency_us);
            }

            Ok(transactions)
        }
        Ok(None) => Err(Status::aborted("connection closed")),
        Err(e) => Err(e),
    };
}
