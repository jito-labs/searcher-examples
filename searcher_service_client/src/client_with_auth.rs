use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use jito_protos::auth::{
    auth_service_client::AuthServiceClient, GenerateAuthChallengeRequest,
    GenerateAuthTokensRequest, RefreshAccessTokenRequest, Role, Token,
};
use log::{error, info};
use solana_sdk::{signature::Signer, signer::keypair::Keypair};
use tokio::runtime::Handle;
use tonic::{
    service::Interceptor,
    transport::{Channel, Endpoint},
    Request, Status,
};

#[derive(Clone)]
pub struct AuthInterceptor {
    url: String,
    auth_keypair: Arc<Keypair>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    access_token_expiration_time: Duration,
    refresh_token_expiration_time: Duration,
}

impl AuthInterceptor {
    pub fn new(url: String, auth_keypair: &Arc<Keypair>) -> AuthInterceptor {
        AuthInterceptor {
            url,
            auth_keypair: auth_keypair.clone(),
            access_token: None,
            refresh_token: None,
            access_token_expiration_time: Duration::default(),
            refresh_token_expiration_time: Duration::default(),
        }
    }

    pub async fn full_auth(&mut self) -> Result<(), Status> {
        info!("performing challenge-response authentication");

        let (access_token, refresh_token) =
            Self::perform_challenge_response(&self.url, &self.auth_keypair).await?;

        self.access_token = Some(access_token.value);
        let access_token_expiration = access_token.expires_at_utc.unwrap();
        self.access_token_expiration_time = Duration::new(
            access_token_expiration.seconds as u64,
            access_token_expiration.nanos as u32,
        );

        self.refresh_token = Some(refresh_token.value);
        let refresh_token_expiration = refresh_token.expires_at_utc.unwrap();
        self.refresh_token_expiration_time = Duration::new(
            refresh_token_expiration.seconds as u64,
            refresh_token_expiration.nanos as u32,
        );
        Ok(())
    }

    pub async fn refresh_auth(&mut self) -> Result<(), Status> {
        info!("refreshing authentication");
        match Self::refresh_access_token(&self.url, &self.refresh_token.as_ref().unwrap()).await {
            Ok(access_token) => {
                self.access_token = Some(access_token.value);
                let access_token_expiration = access_token.expires_at_utc.unwrap();
                self.access_token_expiration_time = Duration::new(
                    access_token_expiration.seconds as u64,
                    access_token_expiration.nanos as u32,
                );
                Ok(())
            }
            Err(e) => {
                error!("error refreshing authentication: {:?}", e);
                info!("re-running authentication");
                self.full_auth().await
            }
        }
    }

    pub async fn maybe_auth(&mut self) -> Result<(), Status> {
        match (self.needs_auth(), self.needs_refresh()) {
            (true, _) => self.full_auth().await,
            (_, true) => self.refresh_auth().await,
            (false, false) => Ok(()),
        }
    }

    /// True if the access token needs refresh
    pub fn needs_refresh(&self) -> bool {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap() >= self.access_token_expiration_time
    }

    /// True if the refresh token has expired, at which point the entire authentication process
    /// needs to happen again.
    pub fn needs_auth(&self) -> bool {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap() >= self.refresh_token_expiration_time
    }

    /// Returns Ok((access_token, refresh_token))
    pub async fn perform_challenge_response(
        url: &String,
        auth_keypair: &Keypair,
    ) -> Result<(Token, Token), Status> {
        let mut auth_client = Self::get_auth_client(url).await?;
        let challenge_resp = auth_client
            .generate_auth_challenge(GenerateAuthChallengeRequest {
                role: Role::Searcher as i32,
                pubkey: auth_keypair.pubkey().as_ref().to_vec(),
            })
            .await?
            .into_inner();

        let challenge = format!("{}-{}", auth_keypair.pubkey(), challenge_resp.challenge);
        let signed_challenge = auth_keypair
            .sign_message(challenge.as_bytes())
            .as_ref()
            .to_vec();

        let tokens = auth_client
            .generate_auth_tokens(GenerateAuthTokensRequest {
                challenge,
                client_pubkey: auth_keypair.pubkey().as_ref().to_vec(),
                signed_challenge,
            })
            .await?
            .into_inner();

        info!("received access and refresh tokens");

        Ok((
            tokens
                .access_token
                .map(|a| a)
                .ok_or_else(|| Status::internal("missing access token"))?,
            tokens
                .refresh_token
                .map(|a| a)
                .ok_or_else(|| Status::internal("missing refresh token"))?,
        ))
    }

    pub async fn refresh_access_token(url: &str, refresh_token: &str) -> Result<Token, Status> {
        let mut auth_client = Self::get_auth_client(url).await?;
        auth_client
            .refresh_access_token(RefreshAccessTokenRequest {
                refresh_token: refresh_token.to_string(),
            })
            .await?
            .into_inner()
            .access_token
            .ok_or_else(|| Status::internal("missing access token"))
    }

    pub async fn get_auth_client(url: &str) -> Result<AuthServiceClient<Channel>, Status> {
        let endpoint = Endpoint::from_shared(url.to_string())
            .map_err(|e| Status::internal(format!("bad authentication uri: {}", e.to_string())))?
            .tls_config(tonic::transport::ClientTlsConfig::new())
            .map_err(|e| Status::internal(format!("TLS error: {}", e.to_string())))?;
        let auth_channel = endpoint.connect().await.map_err(|e| {
            Status::internal(format!(
                "error connecting to auth service: {}",
                e.to_string()
            ))
        })?;
        Ok(AuthServiceClient::new(auth_channel))
    }
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        let handle = Handle::current();
        let _guard = handle.enter();

        futures::executor::block_on(self.maybe_auth())?;

        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", self.access_token.as_ref().unwrap())
                .parse()
                .unwrap(),
        );

        Ok(request)
    }
}
