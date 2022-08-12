use std::{
    net::IpAddr,
    ops::Sub,
    str::FromStr,
    sync::{Arc, Mutex},
    thread::sleep as std_sleep,
    time::Duration,
};

use chrono::{NaiveDateTime, Utc};
use jito_protos::auth::{
    auth_service_client::AuthServiceClient, GenerateAuthChallengeRequest,
    GenerateAuthTokensRequest, RefreshAccessTokenRequest, Role,
};
use jwt::{AlgorithmType, Header, PKeyWithDigest, Token, Verified, VerifyWithKey};
use log::*;
use openssl::pkey::Public;
use serde::{Deserialize, Serialize};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use tokio::{task::JoinHandle, time::sleep};
use tonic::{metadata::MetadataMap, service::Interceptor, transport::Channel, Request, Status};

#[derive(Clone)]
pub struct TokenAuthenticator {
    /// The key used to verify tokens.
    verifying_key: Arc<PKeyWithDigest<Public>>,

    /// The role token's are expected to be encoded with.
    allowed_role: Role,

    /// The tokens' expected signing algo.
    expected_signing_algo: AlgorithmType,

    /// Bypasses the role check. Still requires a correctly formatted token to be present.
    disable_auth: bool,
}

impl TokenAuthenticator {
    pub fn new(
        verifying_key: Arc<PKeyWithDigest<Public>>,
        allowed_role: Role,
        expected_signing_algo: AlgorithmType,
        disable_auth: bool,
    ) -> Self {
        Self {
            verifying_key,
            allowed_role,
            expected_signing_algo,
            disable_auth,
        }
    }

    fn jwt_from_header(
        &self,
        meta: &MetadataMap,
    ) -> Result<Token<Header, DeSerClaims, Verified>, Status> {
        if let Some(auth_header) = meta.get(AUTHORIZATION_HEADER) {
            let auth_header = auth_header.to_str().map_err(|e| {
                warn!("error parsing authorization header {}", e);
                Status::invalid_argument("Failed to parse authorization header.")
            })?;

            if !auth_header.starts_with(BEARER) {
                return Err(Status::permission_denied(
                    "Invalid authorization header format. Must conform to `Authorization: Bearer ${token}`.",
                ));
            }

            let split: Vec<&str> = auth_header.split(BEARER).collect();
            if split.len() != 2 {
                return Err(Status::permission_denied("Missing jwt token."));
            }

            let jwt_token: Token<Header, DeSerClaims, Verified> =
                VerifyWithKey::verify_with_key(split[1], self.verifying_key.as_ref()).map_err(
                    |e| {
                        warn!("error verifying token: {}", e);
                        Status::permission_denied("Token failed verification.")
                    },
                )?;

            // This shouldn't fail since the token passed verification.
            assert_eq!(jwt_token.header().algorithm, self.expected_signing_algo);

            Ok(jwt_token)
        } else {
            Err(Status::permission_denied(
                "The authorization header is missing.",
            ))
        }
    }
}

const AUTHORIZATION_HEADER: &str = "authorization";
const BEARER: &str = "Bearer ";

impl Interceptor for TokenAuthenticator {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        let jwt_token = self.jwt_from_header(req.metadata())?;
        let claims: Claims = jwt_token.claims().into();

        // Bypass auth, this code path should only get hit in testing environments!
        if self.disable_auth {
            req.extensions_mut().insert(claims.client_pubkey);
            return Ok(req);
        }

        if claims.role != self.allowed_role {
            return Err(Status::permission_denied(
                "Client does not have the correct role to access this resource.",
            ));
        }

        Ok(req)
    }
}

/// What the JWT token will be encoded with.
#[derive(Copy, Clone)]
pub struct Claims {
    /// Expected IP of the client.
    pub client_ip: IpAddr,

    /// Expected public key of the client.
    pub client_pubkey: Pubkey,

    /// Client's role.
    pub role: Role,

    /// This token's expirations.
    pub expires_at_utc: NaiveDateTime,
}

impl Claims {
    pub fn is_expired(&self) -> bool {
        self.expires_at_utc.le(&Utc::now().naive_utc())
    }
}

// TODO: Handle these unwraps better just in case we deploy buggy code that issues seemingly valid tokens.
impl From<&DeSerClaims> for Claims {
    fn from(de_ser_claims: &DeSerClaims) -> Self {
        Self {
            client_ip: IpAddr::from_str(&de_ser_claims.client_ip).unwrap(),
            client_pubkey: Pubkey::from_str(&de_ser_claims.client_pubkey).unwrap(),
            role: Role::from_i32(de_ser_claims.role).unwrap(),
            expires_at_utc: NaiveDateTime::from_timestamp(de_ser_claims.expires_at_unix_ts, 0),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct DeSerClaims {
    pub client_ip: String,
    pub client_pubkey: String,
    pub role: i32,
    pub expires_at_unix_ts: i64,
}

impl From<Claims> for DeSerClaims {
    fn from(claims: Claims) -> Self {
        Self {
            client_ip: claims.client_ip.to_string(),
            client_pubkey: claims.client_pubkey.to_string(),
            role: claims.role as i32,
            expires_at_unix_ts: claims.expires_at_utc.timestamp(),
        }
    }
}

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
