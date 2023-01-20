use crate::client_with_auth::AuthInterceptor;
use jito_protos::searcher::searcher_service_client::SearcherServiceClient;
use solana_sdk::signature::Keypair;
use std::sync::Arc;
use thiserror::Error;
use tonic::codegen::http::uri::InvalidUri;
use tonic::codegen::InterceptedService;
use tonic::transport::{Channel, Endpoint};
use tonic::{transport, Status};

pub mod client_with_auth;

#[derive(Debug, Error)]
pub enum BlockEngineConnectionError {
    #[error("transport error {0}")]
    TransportError(#[from] transport::Error),
    #[error("client error {0}")]
    ClientError(#[from] Status),
    #[error("Bad URI path {0}")]
    InvalidUri(#[from] InvalidUri),
}

pub type BlockEngineConnectionResult<T> = Result<T, BlockEngineConnectionError>;

pub async fn get_searcher_client(
    block_engine_url: &str,
    auth_keypair: &Arc<Keypair>,
) -> BlockEngineConnectionResult<SearcherServiceClient<InterceptedService<Channel, AuthInterceptor>>>
{
    let client_interceptor = AuthInterceptor::new(block_engine_url.to_string(), auth_keypair);

    let searcher_channel = Endpoint::from_shared(block_engine_url.to_string())?
        .tls_config(tonic::transport::ClientTlsConfig::new())?
        .connect()
        .await?;
    let searcher_client =
        SearcherServiceClient::with_interceptor(searcher_channel, client_interceptor);

    Ok(searcher_client)
}
