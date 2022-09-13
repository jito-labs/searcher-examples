use std::cmp::min;
use std::collections::HashSet;
use std::{path::Path, result, str::FromStr, sync::Arc, time::Duration};

use bincode::serialize;
use clap::Parser;
use futures_util::StreamExt;
use jito_protos::auth::auth_service_client::AuthServiceClient;
use jito_protos::auth::Role;
use jito_protos::bundle::Bundle;
use jito_protos::packet::Meta as ProtoMeta;
use jito_protos::packet::Packet as ProtoPacket;
use jito_protos::searcher::{
    searcher_service_client::SearcherServiceClient, ConnectedLeadersRequest,
    NextScheduledLeaderRequest, PendingTxNotification, PendingTxSubscriptionRequest,
    SendBundleRequest,
};
use log::*;
use searcher_service_client::token_authenticator::ClientInterceptor;
use solana_client::client_error::ClientError;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_sdk::hash::Hash;
use solana_sdk::packet::{Packet, PACKET_DATA_SIZE};
use solana_sdk::signature::Signer;
use solana_sdk::transaction::Transaction;
use solana_sdk::transaction::VersionedTransaction;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair},
};
use spl_memo::build_memo;
use thiserror::Error;
use tokio::{runtime::Builder, time::interval};
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;
use tonic::{transport::Endpoint, Status};

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

    /// Path to keypair file
    #[clap(long, env)]
    keypair_file: String,

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
}

pub struct BundledTransactions {
    versioned_txs: Vec<VersionedTransaction>,
}

type Result<T> = result::Result<T, BackrunError>;

async fn create_grpc_channel(url: &str) -> Result<Channel> {
    let mut endpoint = Endpoint::from_shared(url.to_string()).expect("invalid url");
    if url.contains("https") {
        endpoint = endpoint.tls_config(tonic::transport::ClientTlsConfig::new())?;
    }
    Ok(endpoint.connect().await?)
}

pub fn versioned_tx_from_packet(p: &ProtoPacket) -> Option<VersionedTransaction> {
    let mut data = [0; PACKET_DATA_SIZE];
    let copy_len = min(data.len(), p.data.len());
    data[..copy_len].copy_from_slice(&p.data[..copy_len]);
    let mut packet = Packet::new(data, Default::default());
    if let Some(meta) = &p.meta {
        packet.meta.size = meta.size as usize;
    }
    packet.deserialize_slice(..).ok()
}

pub fn proto_packet_from_versioned_tx(tx: &VersionedTransaction) -> ProtoPacket {
    let data = serialize(tx).expect("serializes");
    let size = data.len() as u64;
    ProtoPacket {
        data,
        meta: Some(ProtoMeta {
            size,
            addr: "".to_string(),
            port: 0,
            flags: None,
            sender_stake: 0,
        }),
    }
}

fn build_bundles(
    maybe_pending_tx_notification: Option<result::Result<PendingTxNotification, Status>>,
    keypair: &Keypair,
    blockhash: &Hash,
) -> Result<Vec<BundledTransactions>> {
    match maybe_pending_tx_notification {
        Some(Ok(pending_txs)) => {
            let num_txs = pending_txs.transactions.len();
            debug!("num_txs: {:?}", num_txs);

            let bundles: Vec<BundledTransactions> = pending_txs
                .transactions
                .into_iter()
                .filter_map(|packet| {
                    let versioned_tx = versioned_tx_from_packet(&packet)?;
                    let backrun_tx =
                        VersionedTransaction::from(Transaction::new_signed_with_payer(
                            &[build_memo(
                                format!(
                                    "jito backrun: {:?}",
                                    versioned_tx.signatures[0].to_string()
                                )
                                .as_bytes(),
                                &[],
                            )],
                            Some(&keypair.pubkey()),
                            &[keypair],
                            blockhash.clone(),
                        ));
                    Some(BundledTransactions {
                        versioned_txs: vec![versioned_tx, backrun_tx],
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
) -> Result<()> {
    let mut futs = vec![];
    for b in bundles {
        let mut searcher_client = searcher_client.clone();
        let packets = b
            .versioned_txs
            .iter()
            .map(|tx| proto_packet_from_versioned_tx(tx))
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
    for r in responses {
        match r {
            Ok(Ok(response)) => {
                info!("got response: {:?}", response.into_inner());
            }
            _ => {
                error!("error sending bundle");
            }
        }
    }
    Ok(())
}

async fn run_searcher_loop(
    mut searcher_client: SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>,
    backrun_pubkey: Pubkey,
    keypair: &Keypair,
    _pubsub_url: String,
    rpc_url: String,
    _message: String,
) -> Result<()> {
    let pending_tx_stream_response = searcher_client
        .subscribe_pending_transactions(PendingTxSubscriptionRequest {
            accounts: vec![backrun_pubkey.to_string()],
        })
        .await?;

    let rpc_client = RpcClient::new(rpc_url);

    let mut pending_tx_stream = pending_tx_stream_response.into_inner();
    let mut last_validators_connected: HashSet<String> = HashSet::new();
    let mut blockhash = rpc_client
        .get_latest_blockhash_with_commitment(CommitmentConfig {
            commitment: CommitmentLevel::Confirmed,
        })
        .await?;

    let mut refresh_blockhash_interval = interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = refresh_blockhash_interval.tick() => {
                let new_connected_validators: HashSet<String> = searcher_client.get_connected_leaders(ConnectedLeadersRequest{}).await?.into_inner().connected_validators.keys().cloned().collect();
                if new_connected_validators != last_validators_connected {
                    info!("connected_validators: {:?}", new_connected_validators);
                    last_validators_connected = new_connected_validators;
                }
                let next_scheduled_leader = searcher_client.get_next_scheduled_leader(NextScheduledLeaderRequest{}).await?.into_inner();
                info!("next_scheduled_leader: {} in {} slots", next_scheduled_leader.next_leader_identity, next_scheduled_leader.next_leader_slot - next_scheduled_leader.current_slot);

                blockhash = rpc_client
                            .get_latest_blockhash_with_commitment(CommitmentConfig {
                                commitment: CommitmentLevel::Confirmed,
                            })
                            .await?;
            }
            maybe_pending_tx_notification = pending_tx_stream.next() => {
                let bundles = build_bundles(maybe_pending_tx_notification, &keypair, &blockhash.0)?;
                let results = send_bundles(&mut searcher_client, &bundles).await;
            }
        }
    }
}

fn main() -> Result<()> {
    // TODO (LB): init with millis
    env_logger::init();
    let args: Args = Args::parse();

    let keypair =
        Arc::new(read_keypair_file(Path::new(&args.keypair_file)).expect("parse kp file"));
    let backrun_pubkey = Pubkey::from_str(&args.backrun_account).unwrap();

    let runtime = Builder::new_multi_thread().enable_all().build().unwrap();
    runtime.block_on(async move {
        let auth_channel = create_grpc_channel(&args.auth_addr).await?;
        let client_interceptor = ClientInterceptor::new(
            AuthServiceClient::new(auth_channel),
            &keypair,
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

        run_searcher_loop(
            searcher_client,
            backrun_pubkey,
            &keypair,
            args.pubsub_url,
            args.rpc_url,
            args.message,
        )
        .await?;

        Ok(())
    })

    //           loop {
    //               tokio::select! {
    //                   maybe_pending_tx_notification = pending_tx_stream.message() => {
    //                       let transactions_of_interest = get_transactions_of_interest(maybe_pending_tx_notification, &pubkey, &valid_blockhashes).expect("gets transactions");
    //                        if !transactions_of_interest.is_empty() {
    //                            let new_backruns = backrun_transaction(transactions_of_interest, &mut searcher_client, &valid_blockhashes, &kp).await.expect("sends bundles");
    //                            backruns.extend(new_backruns);
    //                        }
    //                   }
    //                   maybe_block = block_notifications.next() => {
    //                       let backrun_stats = update_block_stats(maybe_block, &mut valid_blockhashes, &mut backruns);
    //                       if connected_leader_slots.contains(&backrun_stats.slot) {
    //                           info!("backrun_stats: {:?}", backrun_stats);
    //                       }
    //                   }
    //                   _ = tick.tick() => {
    //                       let next_leader_info = searcher_client.get_next_scheduled_leader(NextScheduledLeaderRequest{}).await.expect("gets next slot").into_inner();
    //                       let slots_until_next = next_leader_info.next_leader_slot - next_leader_info.current_slot;
    //                       info!("next leader slot in {:?} slots, pubkey: {:?}", slots_until_next, next_leader_info.next_leader_identity);
    //
    //                       let leader_schedule = searcher_client.get_connected_leaders(ConnectedLeadersRequest{}).await.expect("get connected leaders").into_inner();
    //                       connected_leader_slots = leader_schedule.connected_validators.values().fold(HashSet::new(), |mut set, slot_list| {
    //                           set.extend(slot_list.slots.clone());
    //                           set
    //                       });
    //                   }
    //               }
    //           }
    //       });
}

// fn update_block_stats(
//     maybe_block: Option<Response<RpcBlockUpdate>>,
//     valid_blockhashes: &mut VecDeque<Hash>,
//     backruns: &mut Vec<BackrunResponse>,
// ) -> BackrunStats {
//     let mut tx_beat_ours = 0;
//     let mut successful_bundles = 0;
//     let mut slot = 0;
//     if let Some(response) = maybe_block {
//         slot = response.context.slot;
//
//         if let Some(block) = response.value.block {
//             valid_blockhashes.push_front(Hash::from_str(&block.blockhash).unwrap());
//             if valid_blockhashes.len() > 100 {
//                 valid_blockhashes.pop_back();
//             }
//
//             if let Some(signatures) = block.signatures {
//                 for backrun in backruns.iter() {
//                     let is_tx_present = signatures.contains(&backrun.tx.signatures[0].to_string());
//                     let is_backrun_present =
//                         signatures.contains(&backrun.backrun_tx.signatures[0].to_string());
//                     match (is_backrun_present, is_tx_present) {
//                         (false, true) => {
//                             tx_beat_ours += 1;
//                         }
//                         (true, true) => {
//                             successful_bundles += 1;
//                         }
//                         _ => {}
//                     }
//                 }
//             }
//         }
//     }
//     let len_before = backruns.len();
//     backruns.retain(|b| b.time_sent.elapsed() < Duration::from_secs(30));
//     let len_after = backruns.len();
//
//     BackrunStats {
//         slot,
//         num_expired: len_before - len_after,
//         num_tx_beat_ours: tx_beat_ours,
//         num_successful_bundles: successful_bundles,
//         num_pending: len_after,
//     }
// }

// #[allow(clippy::await_holding_lock)]
// async fn backrun_transaction(
//     transactions_of_interest: Vec<VersionedTransaction>,
//     searcher_client: &mut SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>,
//     valid_blockhashes: &VecDeque<Hash>,
//     kp: &Arc<Keypair>,
// ) -> Result<Vec<BackrunResponse>, Status> {
//     let blockhash = *valid_blockhashes.front().unwrap();
//     info!("using blockhash {}", blockhash);
//     let mut results = Vec::new();
//
//     for tx in transactions_of_interest.into_iter() {
//         let kp = kp.clone();
//         let time_sent = Instant::now();
//         let backrun_tx = VersionedTransaction::from(Transaction::new_signed_with_payer(
//             &[build_memo(
//                 format!("jito backrun: {:?}", tx.signatures[0].to_string()).as_bytes(),
//                 &[],
//             )],
//             Some(&kp.pubkey()),
//             &[kp.as_ref()],
//             blockhash,
//         ));
//         let bundle = SendBundleRequest {
//             bundle: Some(Bundle {
//                 header: Some(Header {
//                     ts: Some(Timestamp::from(SystemTime::now())),
//                 }),
//                 packets: vec![
//                     proto_packet_from_versioned_tx(&tx),
//                     proto_packet_from_versioned_tx(&backrun_tx),
//                 ],
//             }),
//         };
//
//         if let Ok(response) = searcher_client.send_bundle(bundle.clone()).await {
//             results.push(BackrunResponse {
//                 bundle,
//                 tx: tx.clone(),
//                 backrun_tx,
//                 uuid: response.into_inner().uuid,
//                 time_sent,
//             });
//         }
//     }
//     Ok(results)
// }
//
// fn get_transactions_of_interest(
//     maybe_pending_tx_notification: Result<Option<PendingTxNotification>, Status>,
//     pubkey: &Pubkey,
//     valid_blockhashes: &VecDeque<Hash>,
// ) -> Result<Vec<VersionedTransaction>, Status> {
//     return match maybe_pending_tx_notification {
//         Ok(Some(pending_tx_notification)) => {
//             let expiration_time_since_epoch = pending_tx_notification
//                 .expiration_time
//                 .as_ref()
//                 .map(|ts| SystemTime::try_from(ts.clone()).unwrap())
//                 .unwrap_or(SystemTime::UNIX_EPOCH)
//                 .duration_since(UNIX_EPOCH)
//                 .unwrap();
//             let server_time_since_epoch = pending_tx_notification
//                 .server_side_ts
//                 .as_ref()
//                 .map(|ts| SystemTime::try_from(ts.clone()).unwrap())
//                 .unwrap_or(SystemTime::UNIX_EPOCH)
//                 .duration_since(UNIX_EPOCH)
//                 .unwrap();
//
//             let transactions: Vec<VersionedTransaction> = pending_tx_notification
//                 .transactions
//                 .iter()
//                 .filter_map(versioned_tx_from_packet)
//                 .filter(|tx| tx.message.static_account_keys().contains(pubkey))
//                 .filter(|tx| valid_blockhashes.contains(tx.message.recent_blockhash()))
//                 .collect();
//
//             if !transactions.is_empty() {
//                 let now_since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
//                 let server_latency_us = now_since_epoch.as_micros() as i128
//                     - server_time_since_epoch.as_micros() as i128;
//                 let time_to_expiration_us = expiration_time_since_epoch.as_micros() as i128
//                     - now_since_epoch.as_micros() as i128;
//
//                 info!("time_to_expiration_us: {:?}", time_to_expiration_us);
//                 info!("server_latency_us {:?}", server_latency_us);
//             }
//
//             Ok(transactions)
//         }
//         Ok(None) => Err(Status::aborted("connection closed")),
//         Err(e) => Err(e),
//     };
// }
