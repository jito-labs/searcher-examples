use std::{sync::Arc, time::Duration};

use futures_util::StreamExt;
use jito_protos::{
    auth::{auth_service_client::AuthServiceClient, Role},
    bundle::{Bundle, BundleResult},
    convert::proto_packet_from_versioned_tx,
    searcher::{
        searcher_service_client::SearcherServiceClient, SendBundleRequest, SendBundleResponse,
    },
};
use log::info;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    signature::{Keypair, Signature},
    transaction::VersionedTransaction,
};
use thiserror::Error;
use tokio::time::timeout;
use tonic::{
    codegen::InterceptedService,
    transport,
    transport::{Channel, Endpoint},
    Response, Status, Streaming,
};

use crate::token_authenticator::ClientInterceptor;

pub mod token_authenticator;

#[derive(Debug, Error)]
pub enum BlockEngineConnectionError {
    #[error("transport error {0}")]
    TransportError(#[from] transport::Error),
    #[error("client error {0}")]
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
        auth_keypair,
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

pub async fn send_bundle_with_confirmation(
    transactions: &[VersionedTransaction],
    rpc_client: &RpcClient,
    searcher_client: &mut SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>,
    bundle_results_subscription: &mut Streaming<BundleResult>,
) -> Result<(), Box<dyn std::error::Error>> {
    let signatures: Vec<Signature> = transactions.iter().map(|tx| tx.signatures[0]).collect();

    let result = send_bundle_no_wait(transactions, searcher_client).await?;

    // grab uuid from block engine + wait for results
    let uuid = result.into_inner().uuid;
    info!("Bundle sent. UUID: {:?}", uuid);

    info!("Waiting for 5 seconds to hear results...");
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
        info!("Transactions in bundle did not land");
    } else {
        info!("Bundle landed successfully");
        for sig in signatures {
            println!("https://solscan.io/tx/{sig}");
        }
    }
    Ok(())
}

pub async fn send_bundle_no_wait(
    transactions: &[VersionedTransaction],
    searcher_client: &mut SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>,
) -> Result<Response<SendBundleResponse>, Status> {
    // convert them to packets + send over
    let packets: Vec<_> = transactions
        .iter()
        .map(proto_packet_from_versioned_tx)
        .collect();

    searcher_client
        .send_bundle(SendBundleRequest {
            bundle: Some(Bundle {
                header: None,
                packets,
            }),
        })
        .await
}
