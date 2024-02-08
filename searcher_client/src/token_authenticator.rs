use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime},
};

use arc_swap::{ArcSwap, ArcSwapAny};
use jito_protos::auth::{
    auth_service_client::AuthServiceClient, GenerateAuthChallengeRequest,
    GenerateAuthTokensRequest, RefreshAccessTokenRequest, Role, Token,
};
use prost_types::Timestamp;
use solana_metrics::datapoint_info;
use solana_sdk::signature::{Keypair, Signer};
use thiserror::Error;
use tokio::{task::JoinHandle, time::sleep};
use tonic::{
    metadata::errors::InvalidMetadataValue,
    service::Interceptor,
    transport::{Channel, Endpoint},
    Request, Status,
};

/// Adds the token to each requests' authorization header.
#[derive(Debug, Error)]
pub enum BlockEngineConnectionError {
    #[error("transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    #[error("client error: {0}")]
    Client(#[from] Status),

    #[error("deserializing error")]
    Deserialization,
}

pub type BlockEngineConnectionResult<T> = Result<T, BlockEngineConnectionError>;

/// Manages refreshing the token in a separate thread.
#[derive(Clone)]
pub struct ClientInterceptor {
    /// The access token from jito that added to each request header.
    bearer_token: Arc<ArcSwap<String>>,
}

impl ClientInterceptor {
    pub async fn new(
        mut auth_service_client: AuthServiceClient<Channel>,
        keypair: Arc<Keypair>,
        role: Role,
        service_name: String,
        exit: Arc<AtomicBool>,
    ) -> BlockEngineConnectionResult<(Self, JoinHandle<()>)> {
        let (
            Token {
                value: access_token,
                expires_at_utc: access_token_expiration,
            },
            refresh_token,
        ) = Self::auth(&mut auth_service_client, &keypair, role).await?;
        let bearer_token = Arc::new(ArcSwap::from_pointee(access_token));

        let refresh_thread_handle = Self::spawn_token_refresh_thread(
            auth_service_client,
            bearer_token.clone(),
            refresh_token,
            access_token_expiration.ok_or(BlockEngineConnectionError::Deserialization)?,
            keypair,
            role,
            service_name,
            exit,
        );

        Ok((Self { bearer_token }, refresh_thread_handle))
    }

    /// Returns (access token, refresh token)
    async fn auth(
        auth_service_client: &mut AuthServiceClient<Channel>,
        keypair: &Keypair,
        role: Role,
    ) -> BlockEngineConnectionResult<(Token, Token)> {
        let pubkey_vec = keypair.pubkey().as_ref().to_vec();
        let challenge_resp = auth_service_client
            .generate_auth_challenge(GenerateAuthChallengeRequest {
                role: role as i32,
                pubkey: pubkey_vec.clone(),
            })
            .await?
            .into_inner();
        let challenge = format!("{}-{}", keypair.pubkey(), challenge_resp.challenge);
        let signed_challenge = keypair.sign_message(challenge.as_bytes()).as_ref().to_vec();

        let tokens = auth_service_client
            .generate_auth_tokens(GenerateAuthTokensRequest {
                challenge,
                client_pubkey: pubkey_vec,
                signed_challenge,
            })
            .await?
            .into_inner();

        Ok((
            tokens
                .access_token
                .ok_or(BlockEngineConnectionError::Deserialization)?,
            tokens
                .refresh_token
                .ok_or(BlockEngineConnectionError::Deserialization)?,
        ))
    }

    /// Periodically updates `bearer_token`
    #[allow(clippy::too_many_arguments)]
    fn spawn_token_refresh_thread(
        mut auth_service_client: AuthServiceClient<Channel>,
        bearer_token: Arc<ArcSwap<String>>,
        initial_refresh_token: Token,
        initial_access_token_expiration: Timestamp,
        keypair: Arc<Keypair>,
        role: Role,
        service_name: String,
        exit: Arc<AtomicBool>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            // refresh token gets us an access token. access token is short-lived
            let mut refresh_token = initial_refresh_token;
            let mut access_token_expiration = initial_access_token_expiration;

            while !exit.load(Ordering::Relaxed) {
                let now = SystemTime::now();

                let refresh_token_ttl =
                    SystemTime::try_from(refresh_token.expires_at_utc.as_ref().unwrap().clone())
                        .unwrap()
                        .duration_since(now)
                        .unwrap_or_default();
                // re-run entire auth workflow if refresh token expiring soon
                if refresh_token_ttl < Duration::from_secs(5 * 60) {
                    let start = Instant::now();
                    let is_error = {
                        if let Ok((new_access_token, new_refresh_token)) =
                            Self::auth(&mut auth_service_client, &keypair, role).await
                        {
                            bearer_token.store(Arc::new(new_access_token.value));
                            access_token_expiration = new_access_token.expires_at_utc.unwrap();
                            refresh_token = new_refresh_token;
                            false
                        } else {
                            true
                        }
                    };
                    datapoint_info!(
                        "token_auth",
                        ("auth_type", "full_auth", String),
                        ("service", service_name, String),
                        ("is_error", is_error, bool),
                        ("latency_us", start.elapsed().as_micros(), i64),
                    );
                    continue;
                }

                let access_token_ttl = SystemTime::try_from(access_token_expiration.clone())
                    .unwrap()
                    .duration_since(now)
                    .unwrap_or_default();
                // re-up the access token if it expires soon
                if access_token_ttl < Duration::from_secs(5 * 60) {
                    let start = Instant::now();
                    let is_error = {
                        if let Ok(refresh_resp) = auth_service_client
                            .refresh_access_token(RefreshAccessTokenRequest {
                                refresh_token: refresh_token.value.clone(),
                            })
                            .await
                        {
                            let access_token = refresh_resp.into_inner().access_token.unwrap();
                            bearer_token.store(Arc::new(access_token.value.clone()));
                            access_token_expiration = access_token.expires_at_utc.unwrap();
                            false
                        } else {
                            true
                        }
                    };

                    datapoint_info!(
                        "token_auth",
                        ("auth_type", "access_token", String),
                        ("service", service_name, String),
                        ("is_error", is_error, bool),
                        ("latency_us", start.elapsed().as_micros(), i64),
                    );
                    continue;
                }

                sleep(Duration::from_secs(5)).await;
            }
        })
    }
}

impl Interceptor for ClientInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        let l_token = ArcSwapAny::load(&self.bearer_token);
        if l_token.is_empty() {
            return Err(Status::invalid_argument("missing bearer token"));
        }
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {l_token}")
                .parse()
                .map_err(|e: InvalidMetadataValue| Status::invalid_argument(e.to_string()))?,
        );

        Ok(request)
    }
}

pub async fn create_grpc_channel(url: String) -> BlockEngineConnectionResult<Channel> {
    let endpoint = match url.starts_with("https") {
        true => Endpoint::from_shared(url)
            .map_err(BlockEngineConnectionError::Transport)?
            .tls_config(tonic::transport::ClientTlsConfig::new())?,
        false => Endpoint::from_shared(url).map_err(BlockEngineConnectionError::Transport)?,
    };
    Ok(endpoint.connect().await?)
}
