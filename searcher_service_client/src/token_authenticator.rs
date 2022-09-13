use std::sync::RwLock;
use std::time::SystemTime;
use std::{result, sync::Arc, time::Duration};

use jito_protos::auth::{
    auth_service_client::AuthServiceClient, GenerateAuthChallengeRequest,
    GenerateAuthTokensRequest, RefreshAccessTokenRequest, Role, Token,
};
use prost_types::Timestamp;
use solana_sdk::signature::{Keypair, Signer};
use tokio::{task::JoinHandle, time::sleep};
use tonic::{service::Interceptor, transport::Channel, Request, Status};

const AUTHORIZATION_HEADER: &str = "authorization";
const BEARER: &str = "Bearer ";

type Result<T> = result::Result<T, Status>;

/// Adds the token to each requests' authorization header.
/// Manages refreshing the token in a separate thread.
#[derive(Clone)]
pub struct ClientInterceptor {
    /// The token added to each request header.
    bearer_token: Arc<RwLock<String>>,
}

impl ClientInterceptor {
    pub async fn new(
        mut auth_service_client: AuthServiceClient<Channel>,
        keypair: &Arc<Keypair>,
        role: Role,
    ) -> Result<Self> {
        let (access_token, refresh_token) =
            Self::auth(&mut auth_service_client, keypair, role).await?;

        let bearer_token = Arc::new(RwLock::new(access_token.value.clone()));

        let _refresh_token_thread = Self::spawn_token_refresh_thread(
            auth_service_client,
            bearer_token.clone(),
            refresh_token,
            access_token.expires_at_utc.unwrap(),
            keypair.clone(),
            role,
        );

        Ok(Self { bearer_token })
    }

    async fn auth(
        auth_service_client: &mut AuthServiceClient<Channel>,
        keypair: &Keypair,
        role: Role,
    ) -> Result<(Token, Token)> {
        let challenge_resp = auth_service_client
            .generate_auth_challenge(GenerateAuthChallengeRequest {
                role: role as i32,
                pubkey: keypair.pubkey().as_ref().to_vec(),
            })
            .await?
            .into_inner();
        let challenge = format!("{}-{}", keypair.pubkey(), challenge_resp.challenge);
        let signed_challenge = keypair.sign_message(challenge.as_bytes()).as_ref().to_vec();

        let tokens = auth_service_client
            .generate_auth_tokens(GenerateAuthTokensRequest {
                challenge,
                client_pubkey: keypair.pubkey().as_ref().to_vec(),
                signed_challenge,
            })
            .await?
            .into_inner();

        Ok((tokens.access_token.unwrap(), tokens.refresh_token.unwrap()))
    }

    fn spawn_token_refresh_thread(
        mut auth_service_client: AuthServiceClient<Channel>,
        bearer_token: Arc<RwLock<String>>,
        refresh_token: Token,
        access_token_expiration: Timestamp,
        keypair: Arc<Keypair>,
        role: Role,
    ) -> JoinHandle<Result<()>> {
        tokio::spawn(async move {
            let mut refresh_token = refresh_token;
            let mut access_token_expiration = access_token_expiration;

            loop {
                let access_token_ttl = SystemTime::try_from(access_token_expiration.clone())
                    .unwrap()
                    .duration_since(SystemTime::now())
                    .unwrap_or_else(|_| Duration::from_secs(0));
                let refresh_token_ttl =
                    SystemTime::try_from(refresh_token.expires_at_utc.as_ref().unwrap().clone())
                        .unwrap()
                        .duration_since(SystemTime::now())
                        .unwrap_or_else(|_| Duration::from_secs(0));

                let does_access_token_expire_soon = access_token_ttl < Duration::from_secs(5 * 60);
                let does_refresh_token_expire_soon =
                    refresh_token_ttl < Duration::from_secs(5 * 60);

                match (
                    does_refresh_token_expire_soon,
                    does_access_token_expire_soon,
                ) {
                    // re-run entire auth workflow is refresh token expiring soon
                    (true, _) => {
                        let (new_access_token, new_refresh_token) =
                            Self::auth(&mut auth_service_client, &keypair, role).await?;

                        *bearer_token.write().unwrap() = new_access_token.value.clone();
                        access_token_expiration = new_access_token.expires_at_utc.unwrap();
                        refresh_token = new_refresh_token;
                    }
                    // re-up the access token if it expires soon
                    (_, true) => {
                        let refresh_resp = auth_service_client
                            .refresh_access_token(RefreshAccessTokenRequest {
                                refresh_token: refresh_token.value.clone(),
                            })
                            .await?
                            .into_inner();
                        let access_token = refresh_resp.access_token.unwrap();
                        *bearer_token.write().unwrap() = access_token.value.clone();
                        access_token_expiration = access_token.expires_at_utc.unwrap();
                    }
                    _ => {
                        sleep(Duration::from_secs(60)).await;
                    }
                }
            }
        })
    }
}

impl Interceptor for ClientInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>> {
        let l_token = self.bearer_token.read().unwrap();
        if !l_token.is_empty() {
            request.metadata_mut().insert(
                AUTHORIZATION_HEADER,
                format!("{}{}", BEARER, l_token).parse().unwrap(),
            );
        }

        Ok(request)
    }
}
