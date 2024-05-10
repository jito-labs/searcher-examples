use std::time::Duration;

use futures_util::StreamExt;
use jito_protos::{
    bundle::BundleResult,
    searcher::{
        mempool_subscription, searcher_service_client::SearcherServiceClient, MempoolSubscription,
        PendingTxNotification, SubscribeBundleResultsRequest, WriteLockedAccountSubscriptionV0,
    },
};
use log::info;
use solana_client::{
    nonblocking::pubsub_client::PubsubClient,
    rpc_config::{RpcBlockSubscribeConfig, RpcBlockSubscribeFilter},
    rpc_response,
    rpc_response::{RpcBlockUpdate, SlotUpdate},
};
use solana_metrics::{datapoint_error, datapoint_info};
use solana_sdk::{
    clock::Slot,
    commitment_config::{CommitmentConfig, CommitmentLevel},
    pubkey::Pubkey,
};
use solana_transaction_status::{TransactionDetails, UiTransactionEncoding};
use tokio::{sync::mpsc::Sender, time::sleep};
use tonic::{
    codegen::{Body, Bytes, StdError},
    Streaming,
};

// slot update subscription loop that attempts to maintain a connection to an RPC server
pub async fn slot_subscribe_loop(pubsub_addr: String, slot_sender: Sender<Slot>) {
    let mut connect_errors: u64 = 0;
    let mut slot_subscribe_errors: u64 = 0;
    let mut slot_subscribe_disconnect_errors: u64 = 0;

    loop {
        sleep(Duration::from_secs(1)).await;

        match PubsubClient::new(&pubsub_addr).await {
            Ok(pubsub_client) => match pubsub_client.slot_updates_subscribe().await {
                Ok((mut slot_update_subscription, _unsubscribe_fn)) => {
                    while let Some(slot_update) = slot_update_subscription.next().await {
                        if let SlotUpdate::FirstShredReceived { slot, timestamp: _ } = slot_update {
                            datapoint_info!("slot_subscribe_slot", ("slot", slot, i64));
                            if slot_sender.send(slot).await.is_err() {
                                datapoint_error!("slot_subscribe_send_error", ("errors", 1, i64));
                                return;
                            }
                        }
                    }
                    slot_subscribe_disconnect_errors += 1;
                    datapoint_error!(
                        "slot_subscribe_disconnect_error",
                        ("errors", slot_subscribe_disconnect_errors, i64)
                    );
                }
                Err(e) => {
                    slot_subscribe_errors += 1;
                    datapoint_error!(
                        "slot_subscribe_error",
                        ("errors", slot_subscribe_errors, i64),
                        ("error_str", e.to_string(), String),
                    );
                }
            },
            Err(e) => {
                connect_errors += 1;
                datapoint_error!(
                    "slot_subscribe_pubsub_connect_error",
                    ("errors", connect_errors, i64),
                    ("error_str", e.to_string(), String)
                );
            }
        }
    }
}

// block subscription loop that attempts to maintain a connection to an RPC server
// NOTE: you must have --rpc-pubsub-enable-block-subscription and relevant flags started
// on your RPC servers for this to work.
pub async fn block_subscribe_loop(
    pubsub_addr: String,
    block_receiver: Sender<rpc_response::Response<RpcBlockUpdate>>,
) {
    let mut connect_errors: u64 = 0;
    let mut block_subscribe_errors: u64 = 0;
    let mut block_subscribe_disconnect_errors: u64 = 0;

    loop {
        sleep(Duration::from_secs(1)).await;

        match PubsubClient::new(&pubsub_addr).await {
            Ok(pubsub_client) => match pubsub_client
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
                .await
            {
                Ok((mut block_update_subscription, _unsubscribe_fn)) => {
                    while let Some(block_update) = block_update_subscription.next().await {
                        datapoint_info!(
                            "block_subscribe_slot",
                            ("slot", block_update.context.slot, i64)
                        );
                        if block_receiver.send(block_update).await.is_err() {
                            datapoint_error!("block_subscribe_send_error", ("errors", 1, i64));
                            return;
                        }
                    }
                    block_subscribe_disconnect_errors += 1;
                    datapoint_error!(
                        "block_subscribe_disconnect_error",
                        ("errors", block_subscribe_disconnect_errors, i64)
                    );
                }
                Err(e) => {
                    block_subscribe_errors += 1;
                    datapoint_error!(
                        "block_subscribe_error",
                        ("errors", block_subscribe_errors, i64),
                        ("error_str", e.to_string(), String),
                    );
                }
            },
            Err(e) => {
                connect_errors += 1;
                datapoint_error!(
                    "block_subscribe_pubsub_connect_error",
                    ("errors", connect_errors, i64),
                    ("error_str", e.to_string(), String)
                );
            }
        }
    }
}

// attempts to maintain connection to searcher service and stream pending transaction notifications over a channel
pub async fn pending_tx_loop<T>(
    mut searcher_client: SearcherServiceClient<T>,
    pending_tx_sender: Sender<PendingTxNotification>,
    backrun_pubkeys: Vec<Pubkey>,
) where
    T: tonic::client::GrpcService<tonic::body::BoxBody> + Send + 'static + Clone,
    T::Error: Into<StdError>,
    T::ResponseBody: Body<Data = Bytes> + Send + 'static,
    <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    <T as tonic::client::GrpcService<tonic::body::BoxBody>>::Future: std::marker::Send,
{
    let _num_searcher_connection_errors: usize = 0;
    let mut num_pending_tx_sub_errors: usize = 0;
    let mut num_pending_tx_stream_errors: usize = 0;
    let mut num_pending_tx_stream_disconnects: usize = 0;

    info!("backrun pubkeys: {:?}", backrun_pubkeys);

    loop {
        sleep(Duration::from_secs(1)).await;

        match searcher_client
            .subscribe_mempool(MempoolSubscription {
                regions: vec![],
                msg: Some(mempool_subscription::Msg::WlaV0Sub(
                    WriteLockedAccountSubscriptionV0 {
                        accounts: backrun_pubkeys.iter().map(|pk| pk.to_string()).collect(),
                    },
                )),
            })
            .await
        {
            Ok(pending_tx_stream_response) => {
                let mut pending_tx_stream = pending_tx_stream_response.into_inner();
                while let Some(maybe_notification) = pending_tx_stream.next().await {
                    match maybe_notification {
                        Ok(notification) => {
                            if pending_tx_sender.send(notification).await.is_err() {
                                datapoint_error!("pending_tx_send_error", ("errors", 1, i64));
                                return;
                            }
                        }
                        Err(e) => {
                            num_pending_tx_stream_errors += 1;
                            datapoint_error!(
                                "searcher_pending_tx_stream_error",
                                ("errors", num_pending_tx_stream_errors, i64),
                                ("error_str", e.to_string(), String)
                            );
                            break;
                        }
                    }
                }
                num_pending_tx_stream_disconnects += 1;
                datapoint_error!(
                    "searcher_pending_tx_stream_disconnect",
                    ("errors", num_pending_tx_stream_disconnects, i64),
                );
            }
            Err(e) => {
                num_pending_tx_sub_errors += 1;
                datapoint_error!(
                    "searcher_pending_tx_sub_error",
                    ("errors", num_pending_tx_sub_errors, i64),
                    ("error_str", e.to_string(), String)
                );
            }
        }
    }
}

pub async fn bundle_results_loop<T>(
    mut searcher_client: SearcherServiceClient<T>,
    bundle_results_sender: Sender<BundleResult>,
) where
    T: tonic::client::GrpcService<tonic::body::BoxBody> + Send + 'static + Clone,
    T::Error: Into<StdError>,
    T::ResponseBody: Body<Data = Bytes> + Send + 'static,
    <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    <T as tonic::client::GrpcService<tonic::body::BoxBody>>::Future: std::marker::Send,
{
    let _connection_errors: usize = 0;
    let mut response_errors: usize = 0;

    loop {
        sleep(Duration::from_millis(1000)).await;
        match searcher_client
            .subscribe_bundle_results(SubscribeBundleResultsRequest {})
            .await
        {
            Ok(resp) => {
                consume_bundle_results_stream(resp.into_inner(), &bundle_results_sender).await;
            }
            Err(e) => {
                response_errors += 1;
                datapoint_error!(
                    "searcher_bundle_results_error",
                    ("errors", response_errors, i64),
                    ("msg", e.to_string(), String)
                );
            }
        }
    }
}

pub async fn consume_bundle_results_stream(
    mut stream: Streaming<BundleResult>,
    bundle_results_sender: &Sender<BundleResult>,
) {
    while let Some(maybe_msg) = stream.next().await {
        match maybe_msg {
            Ok(msg) => {
                if let Err(e) = bundle_results_sender.send(msg).await {
                    datapoint_error!(
                        "searcher_bundle_results_error",
                        ("errors", 1, i64),
                        ("msg", e.to_string(), String)
                    );
                    return;
                }
            }
            Err(e) => {
                datapoint_error!(
                    "searcher_bundle_results_error",
                    ("errors", 1, i64),
                    ("msg", e.to_string(), String)
                );
                return;
            }
        }
    }
}
