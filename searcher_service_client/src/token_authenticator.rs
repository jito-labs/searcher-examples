use std::{
    ops::Sub,
    sync::{Arc, Mutex},
    thread::sleep as std_sleep,
    time::Duration,
};

use chrono::Utc;
use jito_protos::auth::{
    auth_service_client::AuthServiceClient, GenerateAuthChallengeRequest,
    GenerateAuthTokensRequest, RefreshAccessTokenRequest, Role,
};
use solana_sdk::{
    signature::{Keypair, Signer},
};
use tokio::{task::JoinHandle, time::sleep};
use tonic::{service::Interceptor, transport::Channel, Request, Status};

const AUTHORIZATION_HEADER: &str = "authorization";
const BEARER: &str = "Bearer ";

/// Adds the token to each requests' authorization header.
/// Manages refreshing the token in a separate thread.
pub struct ClientInterceptor {
    /// The token added to each request header.
    bearer_token: Arc<Mutex<String>>,
}

impl ClientInterceptor {
    pub fn new(
        auth_service_client: AuthServiceClient<Channel>,
        keypair: Arc<Keypair>,
        role: Role,
    ) -> Self {
        let bearer_token = Arc::new(Mutex::new(String::default()));
        let _refresh_token_thread = Self::spawn_token_refresh_thread(
            auth_service_client,
            bearer_token.clone(),
            keypair,
            role,
        );
        std_sleep(Duration::from_secs(3));

        Self { bearer_token }
    }

    fn spawn_token_refresh_thread(
        mut auth_service_client: AuthServiceClient<Channel>,
        bearer_token: Arc<Mutex<String>>,
        keypair: Arc<Keypair>,
        role: Role,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let challenge_resp = auth_service_client
                    .generate_auth_challenge(GenerateAuthChallengeRequest {
                        role: role as i32,
                        pubkey: keypair.pubkey().as_ref().to_vec(),
                    })
                    .await
                    .unwrap()
                    .into_inner();

                let challenge = format!("{}-{}", keypair.pubkey(), challenge_resp.challenge);
                let signed_challenge = keypair.sign_message(challenge.as_bytes()).as_ref().to_vec();

                let tokens_resp = auth_service_client
                    .generate_auth_tokens(GenerateAuthTokensRequest {
                        challenge,
                        client_pubkey: keypair.pubkey().as_ref().to_vec(),
                        signed_challenge,
                    })
                    .await
                    .unwrap()
                    .into_inner();

                let access_token = tokens_resp.access_token.unwrap();
                *bearer_token.lock().unwrap() = access_token.value.clone();
                let mut access_token_expiry = access_token.expires_at_utc.unwrap().seconds;

                let refresh_token = tokens_resp.refresh_token.unwrap();
                let refresh_token_expiry = refresh_token.expires_at_utc.unwrap().seconds;

                loop {
                    let now = Utc::now().timestamp();
                    let sleep_for = access_token_expiry.sub(&now);
                    sleep(Duration::from_secs(sleep_for as u64)).await;

                    // Refresh token has expired, regenerate tokens.
                    let now = Utc::now().timestamp();
                    if now.ge(&refresh_token_expiry) {
                        break;
                    }

                    let refresh_resp = auth_service_client
                        .refresh_access_token(RefreshAccessTokenRequest {
                            refresh_token: refresh_token.value.clone(),
                        })
                        .await
                        .unwrap()
                        .into_inner();

                    let access_token = refresh_resp.access_token.unwrap();
                    *bearer_token.lock().unwrap() = access_token.value.clone();
                    access_token_expiry = access_token.expires_at_utc.unwrap().seconds;
                }
            }
        })
    }
}

impl Interceptor for ClientInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        let l_token = self.bearer_token.lock().unwrap();
        if !l_token.is_empty() {
            request.metadata_mut().insert(
                AUTHORIZATION_HEADER,
                format!("{}{}", BEARER, l_token).parse().unwrap(),
            );
        }

        Ok(request)
    }
}
