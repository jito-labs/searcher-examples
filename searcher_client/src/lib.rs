use std::{str::FromStr, sync::Arc, time::Duration};

use futures_util::StreamExt;
use jito_protos::{
    auth::{auth_service_client::AuthServiceClient, Role},
    bundle::Bundle,
    convert::proto_packet_from_versioned_tx,
    searcher::{
        searcher_service_client::SearcherServiceClient, NextScheduledLeaderRequest,
        SendBundleRequest, SubscribeBundleResultsRequest,
    },
};
use log::info;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair, Signature, Signer},
    system_instruction::transfer,
    transaction::{Transaction, VersionedTransaction},
};
use thiserror::Error;
use tokio::time::{sleep, timeout};
use tonic::{
    codegen::InterceptedService,
    transport,
    transport::{Channel, Endpoint},
    Status,
};

use crate::token_authenticator::ClientInterceptor;

pub mod token_authenticator;

#[derive(Debug, Error)]
pub enum BlockEngineConnectionError {
    #[error("transport error {0}")]
    TransportError(#[from] transport::Error),
    #[error("whirlpools error {0}")]
    ClientError(#[from] Status),
}

pub type BlockEngineConnectionResult<T> = Result<T, BlockEngineConnectionError>;

pub async fn get_searcher_client(
    block_engine_url: &str,
    auth_keypair: &Arc<Keypair>,
) -> BlockEngineConnectionResult<
    SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>,
> {
    let auth_channel = create_grpc_channel(block_engine_url).await?;
    let client_interceptor = ClientInterceptor::new(
        AuthServiceClient::new(auth_channel),
        &auth_keypair,
        Role::Searcher,
    )
    .await?;

    let searcher_channel = create_grpc_channel(block_engine_url).await?;
    let searcher_client =
        SearcherServiceClient::with_interceptor(searcher_channel, client_interceptor);
    Ok(searcher_client)
}

pub async fn create_grpc_channel(url: &str) -> BlockEngineConnectionResult<Channel> {
    let mut endpoint = Endpoint::from_shared(url.to_string()).expect("invalid url");
    if url.contains("https") {
        endpoint = endpoint.tls_config(tonic::transport::ClientTlsConfig::new())?;
    }
    Ok(endpoint.connect().await?)
}

/// Builds and send bundle, attaching the tip and waiting until the next leader
pub async fn build_and_send_bundle(
    rpc_client: &RpcClient,
    bundle: Vec<VersionedTransaction>,
    keypair_path: String,
    tip_lamports: u64,
    tip_account: String,
    client: &mut SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>,
) {
    let payer_keypair = read_keypair_file(keypair_path).expect("reads keypair at path");
    let tip_account = Pubkey::from_str(&tip_account).expect("valid pubkey for tip account");
    let balance = rpc_client
        .get_balance(&payer_keypair.pubkey())
        .await
        .expect("reads balance");

    info!(
        "payer public key: {:?} lamports: {:?}",
        payer_keypair.pubkey(),
        balance
    );

    let mut bundle_results_subscription = client
        .subscribe_bundle_results(SubscribeBundleResultsRequest {})
        .await
        .expect("subscribe to bundle results")
        .into_inner();

    // wait for jito-solana leader slot
    let mut is_leader_slot = false;
    while !is_leader_slot {
        let next_leader = client
            .get_next_scheduled_leader(NextScheduledLeaderRequest {})
            .await
            .expect("gets next scheduled leader")
            .into_inner();
        let num_slots = next_leader.next_leader_slot - next_leader.current_slot;
        is_leader_slot = num_slots <= 2;
        info!("next jito leader slot in {} slots", num_slots);
        sleep(Duration::from_millis(500)).await;
    }

    // build + sign the transactions
    let blockhash = rpc_client
        .get_latest_blockhash()
        .await
        .expect("get blockhash");

    let tip_tx = VersionedTransaction::from(Transaction::new_signed_with_payer(
        &[transfer(
            &payer_keypair.pubkey(),
            &tip_account,
            tip_lamports,
        )],
        Some(&payer_keypair.pubkey()),
        &[&payer_keypair],
        blockhash.clone(),
    ));
    let txs = [bundle, vec![tip_tx]].concat();

    let signatures: Vec<Signature> = txs.iter().map(|tx| tx.signatures[0]).collect();

    // convert them to packets + send over
    let packets: Vec<_> = txs.iter().map(proto_packet_from_versioned_tx).collect();
    let result = client
        .send_bundle(SendBundleRequest {
            bundle: Some(Bundle {
                header: None,
                packets,
            }),
        })
        .await
        .expect("sends bundle");

    // grab uuid from block engine + wait for results
    let uuid = result.into_inner().uuid;
    info!("bundle sent uuid: {:?}", uuid);
    info!("waiting for 5 seconds to hear results...");
    while let Ok(Some(Ok(results))) =
        timeout(Duration::from_secs(5), bundle_results_subscription.next()).await
    {
        info!("bundle results: {:?}", results);
    }

    let futs: Vec<_> = signatures
        .iter()
        .map(|sig| {
            rpc_client.get_signature_status_with_commitment(sig, CommitmentConfig::processed())
        })
        .collect();
    let results = futures::future::join_all(futs).await;
    if !results.iter().all(|r| matches!(r, Ok(Some(Ok(()))))) {
        info!("transactions in bundle did not land :(");
    } else {
        info!("bundle landed successfully!!");
        for sig in signatures {
            info!("https://solscan.io/tx/{}", sig);
        }
    }
}
