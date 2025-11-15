use std::sync::Arc;
use std::time::{Duration, Instant};

use solana_keypair::Keypair;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_pubkey::Pubkey;
use tonic::transport::Channel;
use tonic::{Request, Status};
use tracing::{debug, error, info};

// Use jito-protos for auth service
use jito_protos::auth;

/// Token authenticator that handles challenge-response authentication
/// This version eliminates background auth threads to prevent stampeding
#[derive(Debug)]
pub struct TokenAuthenticator {
    keypair: Arc<Keypair>,
}

/// Authentication result containing token and metadata
#[derive(Debug, Clone)]
pub struct AuthToken {
    pub token: String,
    pub created_at: Instant,
    pub expires_in: Duration,
}

impl AuthToken {
    /// Check if the token is still valid (with some buffer for network delays)
    pub fn is_valid(&self) -> bool {
        let buffer = Duration::from_secs(30); // 30 second buffer
        self.created_at.elapsed() < self.expires_in.saturating_sub(buffer)
    }

    /// Get remaining time until token expires
    pub fn time_until_expiry(&self) -> Duration {
        self.expires_in.saturating_sub(self.created_at.elapsed())
    }
}

/// Errors that can occur during authentication
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("gRPC error: {0}")]
    Grpc(#[from] Status),
    #[error("Invalid challenge response")]
    InvalidChallenge,
    #[error("Authentication timeout")]
    Timeout,
    #[error("Invalid token format")]
    InvalidToken,
}

impl TokenAuthenticator {
    /// Create a new token authenticator
    /// 
    /// IMPORTANT: This version does NOT spawn background auth refresh threads
    /// to eliminate the stampeding issue. Authentication is done on-demand only.
    pub fn new(keypair: Arc<Keypair>) -> Self {
        info!("Creating token authenticator for pubkey: {}", keypair.pubkey());
        Self { keypair }
    }

    /// Get an authentication token using challenge-response flow
    /// This is the main method that performs on-demand authentication
    pub async fn get_auth_token(&self, channel: &Channel) -> Result<String, AuthError> {
        info!("Starting authentication flow for pubkey: {}", self.keypair.pubkey());
        let auth_start = Instant::now();

        // Create a single auth client and reuse it for both requests
        info!("Creating single auth service client...");
        let mut auth_client = auth::auth_service_client::AuthServiceClient::new(channel.clone());

        // Step 1: Get challenge from auth service
        info!("Step 1: Requesting challenge from auth service...");
        let challenge = match self.get_challenge_with_client(&mut auth_client).await {
            Ok(challenge) => {
                info!("Challenge received successfully, {} characters", challenge.len());
                challenge
            },
            Err(e) => {
                error!("Failed to get challenge: {:?}", e);
                return Err(e);
            }
        };
        
        // Step 2: Sign the challenge (FIXED: now signs pubkey-challenge like the old version)
        info!("Step 2: Signing challenge...");
        let signature = self.sign_challenge(&challenge);
        info!("Challenge signed successfully");
        
        // Step 3: Exchange signature for auth token using the same client
        info!("Step 3: Exchanging signature for auth token...");
        let full_challenge = format!("{}-{}", self.keypair.pubkey(), challenge);
        let token = match self.exchange_signature_for_token_with_client(&mut auth_client, full_challenge, signature).await {
            Ok(token) => {
                info!("Auth token received successfully");
                token
            },
            Err(e) => {
                error!("Failed to exchange signature for token: {:?}", e);
                return Err(e);
            }
        };
        
        let auth_duration = auth_start.elapsed();
        info!("Authentication completed successfully in {:?}", auth_duration);
        
        Ok(token)
    }

    /// Get an authentication token with full metadata
    pub async fn get_auth_token_with_metadata(&self, channel: &Channel) -> Result<AuthToken, AuthError> {
        let token = self.get_auth_token(channel).await?;
        
        Ok(AuthToken {
            token,
            created_at: Instant::now(),
            expires_in: Duration::from_secs(300), // 5 minutes default
        })
    }

    /// Step 1: Get challenge from the auth service using provided client
    async fn get_challenge_with_client(&self, auth_client: &mut auth::auth_service_client::AuthServiceClient<tonic::transport::Channel>) -> Result<String, AuthError> {
        info!("Sending GenerateAuthChallengeRequest...");
        info!("Using role: {:?} (value: {})", auth::Role::Searcher, auth::Role::Searcher as i32);
        info!("Using pubkey: {}", self.keypair.pubkey());
        
        let request = Request::new(auth::GenerateAuthChallengeRequest {
            role: auth::Role::Searcher as i32,
            pubkey: self.keypair.pubkey().to_bytes().to_vec(),
        });
        
        let response = match tokio::time::timeout(
            Duration::from_secs(10),
            auth_client.generate_auth_challenge(request)
        ).await {
            Ok(Ok(response)) => {
                info!("Challenge request successful");
                response
            },
            Ok(Err(status)) => {
                error!("gRPC error during challenge request: {:?}", status);
                return Err(AuthError::Grpc(status));
            },
            Err(_) => {
                error!("Challenge request timed out");
                return Err(AuthError::Timeout);
            }
        };
        
        let challenge = response.into_inner().challenge;
        
        if challenge.is_empty() {
            error!("Received empty challenge");
            return Err(AuthError::InvalidChallenge);
        }
        
        info!("Received challenge: '{}' ({} characters)", challenge, challenge.len());
        info!("Challenge as bytes: {:?}", challenge.as_bytes());
        Ok(challenge)
    }

    /// Step 2: Sign the challenge with our keypair
    fn sign_challenge(&self, challenge: &str) -> Signature {
        debug!("Signing challenge");
        
        // Concatenate pubkey with challenge like the old version
        let full_challenge = format!("{}-{}", self.keypair.pubkey(), challenge);
        let challenge_bytes = full_challenge.as_bytes();
        
        info!("Signing message: pubkey-challenge format ({} bytes)", challenge_bytes.len());
        info!("Full challenge to sign: '{}'", full_challenge);
        info!("Challenge bytes to sign: {:?}", challenge_bytes);
        
        self.keypair.sign_message(challenge_bytes)
    }

    /// Step 3: Exchange signed challenge for auth token using provided client
    async fn exchange_signature_for_token_with_client(
        &self,
        auth_client: &mut auth::auth_service_client::AuthServiceClient<tonic::transport::Channel>,
        challenge: String, // full pubkey-challenge string
        signature: Signature,
    ) -> Result<String, AuthError> {
        info!("Sending GenerateAuthTokensRequest...");
        info!("Sending challenge: '{}' ({} characters)", challenge, challenge.len());
        info!("Challenge as bytes: {:?}", challenge.as_bytes());
        info!("Signature: {:?}", signature.as_ref());
        info!("Client pubkey: {}", self.keypair.pubkey());
        
        let request = Request::new(auth::GenerateAuthTokensRequest {
            challenge: challenge.clone(),
            client_pubkey: self.keypair.pubkey().to_bytes().to_vec(),
            signed_challenge: signature.as_ref().to_vec(),
        });
        
        let response = match tokio::time::timeout(
            Duration::from_secs(10),
            auth_client.generate_auth_tokens(request)
        ).await {
            Ok(Ok(response)) => {
                info!("Token exchange successful");
                response
            },
            Ok(Err(status)) => {
                error!("gRPC error during token exchange: {:?}", status);
                error!("Failed with challenge: '{}'", challenge);
                return Err(AuthError::Grpc(status));
            },
            Err(_) => {
                error!("Token exchange timed out");
                return Err(AuthError::Timeout);
            }
        };
        
        let token_response = response.into_inner();
        
        if token_response.access_token.is_none() || token_response.access_token.as_ref().unwrap().value.is_empty() {
            error!("Received empty or invalid auth token");
            return Err(AuthError::InvalidToken);
        }
        
        let token = token_response.access_token.unwrap().value;
        info!("Auth token received, length: {} characters", token.len());
        Ok(token)
    }

    /// Verify that we can authenticate with the given channel
    pub async fn verify_authentication(&self, channel: &Channel) -> Result<(), AuthError> {
        debug!("Verifying authentication capability");
        
        let _token = self.get_auth_token(channel).await?;
        
        info!("Authentication verification successful");
        Ok(())
    }

    /// Get the public key being used for authentication
    pub fn pubkey(&self) -> Pubkey {
        self.keypair.pubkey()
    }
}

/// Utility function to create a token authenticator
pub fn create_token_authenticator(keypair: Arc<Keypair>) -> TokenAuthenticator {
    TokenAuthenticator::new(keypair)
}

/// Authentication helper that can be used across multiple connections
/// This version ensures no background threads are spawned
#[derive(Debug)]
pub struct AuthenticationManager {
    authenticator: TokenAuthenticator,
}

impl AuthenticationManager {
    /// Create a new authentication manager
    pub fn new(keypair: Arc<Keypair>) -> Self {
        Self {
            authenticator: TokenAuthenticator::new(keypair),
        }
    }

    /// Authenticate with multiple channels concurrently
    /// This can be useful when establishing multiple connections
    pub async fn authenticate_channels(
        &self,
        channels: Vec<Channel>,
    ) -> Result<Vec<String>, AuthError> {
        debug!("Authenticating {} channels concurrently", channels.len());
        
        let mut handles = Vec::new();
        
        for channel in channels {
            // Clone the keypair to avoid lifetime issues
            let keypair = self.authenticator.keypair.clone();
            let handle = tokio::spawn(async move {
                let auth = TokenAuthenticator::new(keypair);
                auth.get_auth_token(&channel).await
            });
            handles.push(handle);
        }
        
        let mut tokens = Vec::new();
        for handle in handles {
            let token = handle.await
                .map_err(|_e| AuthError::InvalidToken)?  // Join error
                .map_err(|e| e)?; // Auth error
            tokens.push(token);
        }
        
        debug!("Successfully authenticated {} channels", tokens.len());
        Ok(tokens)
    }

    /// Get the public key being used for authentication
    pub fn pubkey(&self) -> Pubkey {
        self.authenticator.pubkey()
    }
}