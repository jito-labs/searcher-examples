use std::sync::Arc;

use jito_protos::searcher::{
    searcher_service_client::SearcherServiceClient, ConnectedLeadersRequest,
    ConnectedLeadersResponse, NextScheduledLeaderRequest, NextScheduledLeaderResponse,
    PendingTxNotification, PendingTxSubscriptionRequest, SendBundleRequest, SendBundleResponse,
};
use solana_sdk::signature::{Keypair, Signer};
use tokio::runtime::Runtime;
use tokio::sync::Mutex;
use tonic::{codegen::InterceptedService, transport::Channel, Response, Status, Streaming};
use tonic::{metadata::MetadataValue, service::Interceptor, Code};

// Auth header keys
pub const MESSAGE_BIN: &str = "message-bin";
pub const PUBKEY_BIN: &str = "public-key-bin";
pub const SIGNATURE_BIN: &str = "signature-bin";

/// Wrapper client that takes care of extracting the auth challenge and retrying requests.
pub struct AuthClient {
    inner: SearcherServiceClient<InterceptedService<Channel, AuthInterceptor>>,
    token: Arc<Mutex<String>>,
    max_retries: usize,
}

impl AuthClient {
    pub fn new(
        inner: SearcherServiceClient<InterceptedService<Channel, AuthInterceptor>>,
        token: Arc<Mutex<String>>,
        max_retries: usize,
    ) -> Self {
        Self {
            inner,
            token,
            max_retries,
        }
    }

    pub async fn get_next_scheduled_leader(
        &mut self,
        req: NextScheduledLeaderRequest,
    ) -> Result<Response<NextScheduledLeaderResponse>, Status> {
        let mut n_retries = 0;
        loop {
            return match self.inner.get_next_scheduled_leader(req.clone()).await {
                Ok(resp) => Ok(resp),
                Err(status) => {
                    if AuthInterceptor::should_retry(
                        &status,
                        self.token.clone(),
                        self.max_retries,
                        n_retries,
                    )
                    .await
                    {
                        n_retries += 1;
                        continue;
                    }
                    Err(status)
                }
            };
        }
    }

    pub async fn get_connected_leaders(
        &mut self,
        req: ConnectedLeadersRequest,
    ) -> Result<Response<ConnectedLeadersResponse>, Status> {
        let mut n_retries = 0;
        loop {
            return match self.inner.get_connected_leaders(req.clone()).await {
                Ok(resp) => Ok(resp),
                Err(status) => {
                    if AuthInterceptor::should_retry(
                        &status,
                        self.token.clone(),
                        self.max_retries,
                        n_retries,
                    )
                    .await
                    {
                        n_retries += 1;
                        continue;
                    }
                    Err(status)
                }
            };
        }
    }

    pub async fn send_bundle(
        &mut self,
        req: SendBundleRequest,
    ) -> Result<Response<SendBundleResponse>, Status> {
        let mut n_retries = 0;
        loop {
            return match self.inner.send_bundle(req.clone()).await {
                Ok(resp) => Ok(resp),
                Err(status) => {
                    if AuthInterceptor::should_retry(
                        &status,
                        self.token.clone(),
                        self.max_retries,
                        n_retries,
                    )
                    .await
                    {
                        n_retries += 1;
                        continue;
                    }
                    Err(status)
                }
            };
        }
    }

    pub async fn subscribe_pending_transactions(
        &mut self,
        req: PendingTxSubscriptionRequest,
    ) -> Result<Response<Streaming<PendingTxNotification>>, Status> {
        let mut n_retries = 0;
        loop {
            return match self.inner.subscribe_pending_transactions(req.clone()).await {
                Ok(resp) => Ok(resp),
                Err(status) => {
                    if AuthInterceptor::should_retry(
                        &status,
                        self.token.clone(),
                        self.max_retries,
                        n_retries,
                    )
                    .await
                    {
                        n_retries += 1;
                        continue;
                    }
                    Err(status)
                }
            };
        }
    }
}

/// Intercepts requests and adds the necessary headers for auth.
#[derive(Clone)]
pub struct AuthInterceptor {
    rt: Arc<Runtime>,
    /// Used to sign the server generated token.
    keypair: Arc<Keypair>,
    token: Arc<Mutex<String>>,
}

impl AuthInterceptor {
    pub fn new(keypair: Arc<Keypair>, token: Arc<Mutex<String>>) -> Self {
        AuthInterceptor {
            rt: Arc::new(Runtime::new().unwrap()),
            keypair,
            token,
        }
    }

    pub async fn should_retry(
        status: &Status,
        token: Arc<Mutex<String>>,
        max_retries: usize,
        n_retries: usize,
    ) -> bool {
        if max_retries == n_retries {
            return false;
        }

        let mut token = token.lock().await;
        if let Some(new_token) = Self::maybe_new_auth_token(status, &token) {
            *token = new_token;
            true
        } else {
            false
        }
    }

    /// Checks to see if the server returned a token to be signed and if it does not equal the current
    /// token then the new token is returned and authentication can be retried.
    fn maybe_new_auth_token(status: &Status, current_token: &str) -> Option<String> {
        if status.code() != Code::Unauthenticated {
            return None;
        }

        let msg = status.message().split_whitespace().collect::<Vec<&str>>();
        if msg.len() != 2 {
            return None;
        }

        if msg[0] != "token:" {
            return None;
        }

        if msg[1] != current_token {
            Some(msg[1].to_string())
        } else {
            None
        }
    }
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, mut request: tonic::Request<()>) -> Result<tonic::Request<()>, Status> {
        let token = self.rt.block_on(self.token.lock()).clone();
        // Prefix with pubkey and hash it in order to ensure BlockEngine doesn't have us sign a malicious transaction.
        let token = format!("{}-{}", self.keypair.pubkey(), token);
        let hashed_token = solana_sdk::hash::hash(token.as_bytes());

        request.metadata_mut().append_bin(
            PUBKEY_BIN,
            MetadataValue::from_bytes(&self.keypair.pubkey().to_bytes()),
        );
        request.metadata_mut().append_bin(
            MESSAGE_BIN,
            MetadataValue::from_bytes(hashed_token.to_bytes().as_slice()),
        );
        request.metadata_mut().append_bin(
            SIGNATURE_BIN,
            MetadataValue::from_bytes(
                self.keypair
                    .sign_message(hashed_token.to_bytes().as_slice())
                    .as_ref(),
            ),
        );

        Ok(request)
    }
}
