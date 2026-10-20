use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::collections::HashMap;

use arc_swap::ArcSwap;
use bincode;
use tokio::sync::{Mutex, Notify};
use tonic::transport::{Channel, Endpoint, ClientTlsConfig};
use tonic::{Request, Streaming};
use tracing::{error, info, warn};

// Re-export only the essential types users need for the searcher client
pub use solana_transaction::Transaction;
pub use solana_pubkey::Pubkey;

// Use jito-protos - import from the correct modules
pub use jito_protos::searcher as proto;
pub use jito_protos::auth;
pub use jito_protos::bundle::{Bundle, BundleResult};
pub use jito_protos::packet::Packet;
pub use jito_protos::searcher::SlotList;

// Re-export the token authenticator module
pub mod token_authenticator;
pub use token_authenticator::*;

/// Errors that can occur in the searcher client
#[derive(Debug, thiserror::Error)]
pub enum SearcherClientError {
    #[error("Transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("gRPC error: {0}")]
    Grpc(#[from] tonic::Status),
    #[error("Authentication failed: {0}")]
    Authentication(String),
    #[error("Connection not ready")]
    NotReady,
    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),
    #[error("Invalid configuration: {0}")]
    Configuration(String),
}

/// Connection state that holds channel and auth token
#[derive(Debug, Clone)]
struct ConnectionState {
    channel: Channel,
    auth_token: String,
    authenticated_at: Instant,
    token_ttl: Duration,
}

impl ConnectionState {
    fn is_valid(&self) -> bool {
        self.authenticated_at.elapsed() < self.token_ttl
    }
}

/// Configuration for the searcher client
#[derive(Debug, Clone)]
pub struct SearcherClientConfig {
    pub endpoint: String,
    pub timeout: Duration,
    pub connect_timeout: Duration,
    pub auth_ttl: Duration,
    pub add_timing_headers: bool,
}

impl Default for SearcherClientConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://frankfurt.mainnet.block-engine.jito.wtf:443".to_string(),
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            auth_ttl: Duration::from_secs(300), // 5 minutes
            add_timing_headers: true, // Enable timing headers by default for debugging
        }
    }
}

/// Main searcher client - fixes auth thread stampeding issue
pub struct SearcherClient {
    //keypair: Arc<solana_keypair::Keypair>,
    config: SearcherClientConfig,
    /// Connection state using ArcSwap for lock-free reads (replaces RwLock)
    connection: ArcSwap<Option<ConnectionState>>,
    /// Prevents multiple threads from connecting simultaneously (fixes stampeding)
    connection_lock: Mutex<()>,
    /// Notification for connection ready events
    connection_ready: Notify,
    /// Token authenticator
    authenticator: TokenAuthenticator,
}

impl SearcherClient {
    /// Create a new searcher client with default configuration
    pub fn new(keypair: Arc<solana_keypair::Keypair>) -> Result<Self, SearcherClientError> {
        Self::new_with_config(keypair, SearcherClientConfig::default())
    }

    /// Create a new searcher client with custom configuration
    pub fn new_with_config(
        keypair: Arc<solana_keypair::Keypair>,
        config: SearcherClientConfig,
    ) -> Result<Self, SearcherClientError> {
        let authenticator = TokenAuthenticator::new(keypair.clone());

        Ok(Self {
            //keypair,
            config,
            connection: ArcSwap::new(Arc::new(None)),
            connection_lock: Mutex::new(()),
            connection_ready: Notify::new(),
            authenticator,
        })
    }

    /// Connect and authenticate with the block engine
    /// This method eliminates auth thread stampeding through proper synchronization
    pub async fn connect(&self) -> Result<(), SearcherClientError> {
        // Fast path: check if we already have a valid connection (lock-free read)
        if let Some(conn_state) = self.connection.load().as_ref().as_ref() {
            if conn_state.is_valid() {
                info!("Reusing existing valid connection");
                return Ok(());
            }
        }

        // Slow path: establish new connection with mutex to prevent stampeding
        let _guard = self.connection_lock.lock().await;

        // Double-check after acquiring lock (another thread might have connected)
        if let Some(conn_state) = self.connection.load().as_ref().as_ref() {
            if conn_state.is_valid() {
                info!("Another thread established connection while waiting");
                return Ok(());
            }
        }

        info!("Establishing new connection to {}", self.config.endpoint);

        // Extract domain name for TLS verification  
        let domain = self.config.endpoint
            .strip_prefix("https://")
            .unwrap_or(&self.config.endpoint)
            .split(':')
            .next()
            .unwrap_or(&self.config.endpoint);

        info!("Using TLS domain for verification: {}", domain);

        // Create gRPC channel with proper TLS configuration including domain name and native roots
        info!("Creating gRPC endpoint...");
        let endpoint = Endpoint::from_shared(self.config.endpoint.clone())
            .map_err(|e| SearcherClientError::Configuration(format!("Invalid endpoint URL: {}", e)))?
            .tls_config(
                tonic::transport::ClientTlsConfig::new()
                    .domain_name(domain)
                    .with_native_roots()  // Use system's native root certificates
            )
            .map_err(|e| SearcherClientError::Configuration(format!("TLS config error: {}", e)))?
            .connect_timeout(self.config.connect_timeout)
            .timeout(self.config.timeout);

        info!("Connecting to gRPC endpoint...");
        let channel = endpoint.connect().await
            .map_err(|e| {
                error!("Failed to establish gRPC channel: {:?}", e);
                SearcherClientError::Transport(e)
            })?;

        info!("gRPC channel established successfully, starting authentication...");

        // Authenticate using the token authenticator
        let auth_token = match self.authenticator.get_auth_token(&channel).await {
            Ok(token) => {
                info!("Authentication successful");
                token
            },
            Err(e) => {
                error!("Authentication failed: {:?}", e);
                return Err(SearcherClientError::Authentication(e.to_string()));
            }
        };

        // Store the new connection state
        let conn_state = ConnectionState {
            channel,
            auth_token,
            authenticated_at: Instant::now(),
            token_ttl: self.config.auth_ttl,
        };

        self.connection.store(Arc::new(Some(conn_state)));
        self.connection_ready.notify_waiters();

        info!("Successfully connected and authenticated");
        Ok(())
    }

    /// Get a ready connection, connecting if necessary
    async fn get_connection(&self) -> Result<ConnectionState, SearcherClientError> {
        // Try to get existing valid connection
        if let Some(conn_state) = self.connection.load().as_ref().as_ref() {
            if conn_state.is_valid() {
                return Ok(conn_state.clone());
            }
        }

        // Need to connect
        self.connect().await?;

        // Should have a connection now
        self.connection
            .load()
            .as_ref()
            .as_ref()
            .ok_or(SearcherClientError::NotReady)
            .map(|conn| conn.clone())
    }

    /// Force reconnection (useful for error recovery)
    pub async fn reconnect(&self) -> Result<(), SearcherClientError> {
        // Clear current connection state
        self.connection.store(Arc::new(None));

        // Establish new connection
        self.connect().await
    }

    /// Check if client is connected
    pub fn is_connected(&self) -> bool {
        self.connection
            .load()
            .as_ref()
            .as_ref()
            .map(|conn| conn.is_valid())
            .unwrap_or(false)
    }

    /// Send a bundle to the block engine
    pub async fn send_bundle(
        &self,
        bundle: &Bundle,
    ) -> Result<String, SearcherClientError> {
        info!("Starting send_bundle operation...");
        
        let conn_state = match self.get_connection().await {
            Ok(state) => {
                info!("Connection state obtained successfully");
                state
            },
            Err(e) => {
                error!("Failed to get connection state: {:?}", e);
                return Err(e);
            }
        };

        info!("Creating searcher service client...");
        let mut client = proto::searcher_service_client::SearcherServiceClient::new(
            conn_state.channel.clone(),
        );

        info!("Preparing send bundle request...");
        let mut request = Request::new(proto::SendBundleRequest {
            bundle: Some(bundle.clone()),
        });

        // Add auth token to metadata
        if let Err(e) = self.add_auth_header(&mut request, &conn_state.auth_token) {
            error!("Failed to add auth header: {:?}", e);
            return Err(e);
        }
        info!("Auth header added successfully");

        // Add timing header for debugging
        if let Err(e) = self.add_timing_header(&mut request) {
            warn!("Failed to add timing header: {:?}", e);
            // Don't fail the request for timing header issues
        }

        info!("Sending bundle via gRPC...");
        let response = match client.send_bundle(request).await {
            Ok(response) => {
                info!("Bundle sent successfully!");
                response
            },
            Err(status) => {
                error!("gRPC error during send_bundle: {:?}", status);
                error!("Status code: {:?}", status.code());
                error!("Status message: {}", status.message());
                return Err(SearcherClientError::Grpc(status));
            }
        };
        
        let uuid = response.into_inner().uuid;
        info!("Bundle UUID: {}", uuid);
        Ok(uuid)
    }

    /// Send a bundle with transactions
    pub async fn send_bundle_with_transactions(
        &self,
        transactions: Vec<solana_transaction::Transaction>,
    ) -> Result<String, SearcherClientError> {
        info!("Converting {} transactions to bundle...", transactions.len());
        
        let bundle = match self.transactions_to_bundle(transactions) {
            Ok(bundle) => {
                info!("Bundle created successfully with {} packets", bundle.packets.len());
                bundle
            },
            Err(e) => {
                error!("Failed to convert transactions to bundle: {:?}", e);
                return Err(e);
            }
        };
        
        info!("Calling send_bundle...");
        self.send_bundle(&bundle).await
    }

    /// Send a bundle to the block engine without authentication
    pub async fn send_bundle_no_auth(
        &self,
        bundle: &Bundle,
    ) -> Result<String, SearcherClientError> {
        info!("Starting send_bundle operation (no authentication)...");
        
        // Extract domain name for TLS verification  
        let domain = self.config.endpoint
            .strip_prefix("https://")
            .unwrap_or(&self.config.endpoint)
            .split(':')
            .next()
            .unwrap_or(&self.config.endpoint);
        
        // Create gRPC channel without authentication
        info!("Creating direct gRPC channel (no auth)...");
        let channel = Channel::from_shared(self.config.endpoint.clone())
            .map_err(|e| SearcherClientError::Configuration(format!("Invalid endpoint URL: {}", e)))?
            .tls_config(
                ClientTlsConfig::new()
                    .domain_name(domain)
                    .with_native_roots()
            )
            .map_err(|e| SearcherClientError::Configuration(format!("TLS config error: {}", e)))?
            .connect_timeout(self.config.connect_timeout)
            .timeout(self.config.timeout)
            .connect()
            .await
            .map_err(|e| {
                error!("Failed to establish gRPC channel: {:?}", e);
                SearcherClientError::Transport(e)
            })?;

        info!("Creating searcher service client...");
        let mut client = proto::searcher_service_client::SearcherServiceClient::new(channel);

        info!("Preparing send bundle request (no auth header)...");
        let mut request = Request::new(proto::SendBundleRequest {
            bundle: Some(bundle.clone()),
        });
        // Note: NO auth header added

        // Add timing header for debugging (even without auth)
        if let Err(e) = self.add_timing_header(&mut request) {
            warn!("Failed to add timing header: {:?}", e);
            // Don't fail the request for timing header issues
        }

        info!("Sending bundle via gRPC (no authentication)...");
        let response = match client.send_bundle(request).await {
            Ok(response) => {
                info!("Bundle sent successfully!");
                response
            },
            Err(status) => {
                error!("gRPC error during send_bundle: {:?}", status);
                error!("Status code: {:?}", status.code());
                error!("Status message: {}", status.message());
                return Err(SearcherClientError::Grpc(status));
            }
        };
        
        let uuid = response.into_inner().uuid;
        info!("Bundle UUID: {}", uuid);
        Ok(uuid)
    }

    /// Send a bundle with transactions (no authentication)
    pub async fn send_bundle_with_transactions_no_auth(
        &self,
        transactions: Vec<solana_transaction::Transaction>,
    ) -> Result<String, SearcherClientError> {
        info!("Converting {} transactions to bundle (no auth)...", transactions.len());
        
        let bundle = match self.transactions_to_bundle(transactions) {
            Ok(bundle) => {
                info!("Bundle created successfully with {} packets", bundle.packets.len());
                bundle
            },
            Err(e) => {
                error!("Failed to convert transactions to bundle: {:?}", e);
                return Err(e);
            }
        };
        
        info!("Calling send_bundle_no_auth...");
        self.send_bundle_no_auth(&bundle).await
    }

    /// Get tip accounts
    pub async fn get_tip_accounts(&self) -> Result<Vec<String>, SearcherClientError> {
        let conn_state = self.get_connection().await?;

        let mut client = proto::searcher_service_client::SearcherServiceClient::new(
            conn_state.channel.clone(),
        );

        let mut request = Request::new(proto::GetTipAccountsRequest {});
        self.add_auth_header(&mut request, &conn_state.auth_token)?;
        
        // Add timing header for debugging
        if let Err(e) = self.add_timing_header(&mut request) {
            warn!("Failed to add timing header: {:?}", e);
        }

        let response = client.get_tip_accounts(request).await?;
        Ok(response.into_inner().accounts)
    }

    /// Get tip accounts (no authentication required) - use working TLS config
    pub async fn get_tip_accounts_no_auth(&self) -> Result<Vec<String>, SearcherClientError> {
        use tonic::transport::{Channel, ClientTlsConfig};
        
        // Extract domain name for TLS verification  
        let domain = self.config.endpoint
            .strip_prefix("https://")
            .unwrap_or(&self.config.endpoint)
            .split(':')
            .next()
            .unwrap_or(&self.config.endpoint);
        
        // Use the TLS configuration with proper domain verification
        let channel = Channel::from_shared(self.config.endpoint.clone())
            .map_err(|e| SearcherClientError::Configuration(format!("Invalid endpoint URL: {}", e)))?
            .tls_config(
                ClientTlsConfig::new()
                    .domain_name(domain)  // Add domain name for proper certificate verification
                    .with_native_roots()  // Use system's native root certificates
            )
            .map_err(|e| SearcherClientError::Configuration(format!("TLS config error: {}", e)))?
            .connect()
            .await?;

        let mut client = proto::searcher_service_client::SearcherServiceClient::new(channel);
        let mut request = Request::new(proto::GetTipAccountsRequest {});
        
        // Add timing header for debugging (even without auth)
        if let Err(e) = self.add_timing_header(&mut request) {
            warn!("Failed to add timing header: {:?}", e);
        }
        
        let response = client.get_tip_accounts(request).await?;
        Ok(response.into_inner().accounts)
    }

    /// Get connected leaders
    pub async fn get_connected_leaders(&self) -> Result<HashMap<String, SlotList>, SearcherClientError> {
        let conn_state = self.get_connection().await?;

        let mut client = proto::searcher_service_client::SearcherServiceClient::new(
            conn_state.channel.clone(),
        );

        let mut request = Request::new(proto::ConnectedLeadersRequest {}); // Fixed struct name
        self.add_auth_header(&mut request, &conn_state.auth_token)?;
        
        // Add timing header for debugging
        if let Err(e) = self.add_timing_header(&mut request) {
            warn!("Failed to add timing header: {:?}", e);
        }

        let response = client.get_connected_leaders(request).await?;
        Ok(response.into_inner().connected_validators)
    }

    /// Get connected leader pubkeys (just the validator identities)
    pub async fn get_connected_leader_pubkeys(&self) -> Result<Vec<String>, SearcherClientError> {
        let connected_leaders = self.get_connected_leaders().await?;
        Ok(connected_leaders.keys().cloned().collect())
    }

    /// Get next scheduled leader
    pub async fn get_next_scheduled_leader(&self) -> Result<proto::NextScheduledLeaderResponse, SearcherClientError> {
        self.get_next_scheduled_leader_with_regions(vec![]).await
    }

    /// Get next scheduled leader with specific regions
    pub async fn get_next_scheduled_leader_with_regions(
        &self,
        regions: Vec<String>,
    ) -> Result<proto::NextScheduledLeaderResponse, SearcherClientError> {
        let conn_state = self.get_connection().await?;

        let mut client = proto::searcher_service_client::SearcherServiceClient::new(
            conn_state.channel.clone(),
        );

        let mut request = Request::new(proto::NextScheduledLeaderRequest {
            regions,
        });
        self.add_auth_header(&mut request, &conn_state.auth_token)?;
        
        // Add timing header for debugging
        if let Err(e) = self.add_timing_header(&mut request) {
            warn!("Failed to add timing header: {:?}", e);
        }

        let response = client.get_next_scheduled_leader(request).await?;
        Ok(response.into_inner())
    }

    /// Subscribe to bundle results
    pub async fn subscribe_bundle_results(
        &self,
    ) -> Result<Streaming<BundleResult>, SearcherClientError> { // Updated type reference
        let conn_state = self.get_connection().await?;

        let mut client = proto::searcher_service_client::SearcherServiceClient::new(
            conn_state.channel.clone(),
        );

        let mut request = Request::new(proto::SubscribeBundleResultsRequest {});
        self.add_auth_header(&mut request, &conn_state.auth_token)?;
        
        // Add timing header for debugging
        if let Err(e) = self.add_timing_header(&mut request) {
            warn!("Failed to add timing header: {:?}", e);
        }

        let response = client.subscribe_bundle_results(request).await?;
        Ok(response.into_inner())
    }

    /// Convert transactions to protobuf bundle
    fn transactions_to_bundle(
        &self,
        transactions: Vec<solana_transaction::Transaction>,
    ) -> Result<Bundle, SearcherClientError> { // Updated return type
        if transactions.len() > 5 {
            return Err(SearcherClientError::Configuration(
                "Bundle cannot contain more than 5 transactions".to_string(),
            ));
        }

        let packets = transactions
            .into_iter()
            .map(|tx| {
                // Serialize transaction using bincode (compatible with Solana)
                let data = bincode::serialize(&tx).map_err(|e| SearcherClientError::Serialization(e))?;
                Ok(Packet { data, meta: None }) // Updated type reference
            })
            .collect::<Result<Vec<_>, SearcherClientError>>()?;

        Ok(Bundle { // Updated type reference
            header: None,
            packets,
        })
    }

    /// Add auth header to request
    fn add_auth_header<T>(
        &self,
        request: &mut Request<T>,
        auth_token: &str,
    ) -> Result<(), SearcherClientError> {
        let auth_header = format!("Bearer {}", auth_token);
        request.metadata_mut().insert(
            "authorization",
            auth_header.parse().map_err(|e| {
                SearcherClientError::Authentication(format!("Invalid auth header: {}", e))
            })?,
        );
        Ok(())
    }

    /// Add timing header to request for debugging latency
    fn add_timing_header<T>(
        &self,
        request: &mut Request<T>,
    ) -> Result<(), SearcherClientError> {
        if !self.config.add_timing_headers {
            return Ok(());
        }

        // Get current time in milliseconds since Unix epoch
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| SearcherClientError::Configuration(format!("System time error: {}", e)))?
            .as_millis();

        let timestamp_header = now.to_string();
        request.metadata_mut().insert(
            "x-received-at",
            timestamp_header.parse().map_err(|e| {
                SearcherClientError::Configuration(format!("Invalid timestamp header: {}", e))
            })?,
        );
        
        info!("Added timing header: x-received-at = {}", now);
        Ok(())
    }
}

/// Retry wrapper for operations that might fail due to connection issues
pub async fn with_retry<F, Fut, T>(
    client: &SearcherClient,
    operation: F,
    max_retries: usize,
) -> Result<T, SearcherClientError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, SearcherClientError>>,
{
    let mut last_error = None;

    for attempt in 1..=max_retries {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(err) => {
                // Check if this is a connection-related error that should trigger retry
                let should_retry = matches!(
                    &err,
                    SearcherClientError::Grpc(status) if matches!(
                        status.code(),
                        tonic::Code::Unavailable
                            | tonic::Code::DeadlineExceeded
                            | tonic::Code::Cancelled
                            | tonic::Code::Unauthenticated
                    )
                ) || matches!(&err, SearcherClientError::NotReady);

                if should_retry && attempt < max_retries {
                    warn!("Operation failed on attempt {}, retrying: {}", attempt, err);

                    // Force reconnection for connection-related errors
                    if let Err(reconnect_err) = client.reconnect().await {
                        error!("Failed to reconnect: {}", reconnect_err);
                    }

                    // Exponential backoff
                    let delay = Duration::from_millis(100 * (1 << (attempt - 1)));
                    tokio::time::sleep(delay).await;
                } else {
                    last_error = Some(err);
                    break;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        SearcherClientError::Authentication("Max retries exceeded".to_string())
    }))
}

/// Convenience function to create a searcher client
pub fn create_searcher_client(keypair: Arc<solana_keypair::Keypair>) -> Result<SearcherClient, SearcherClientError> {
    SearcherClient::new(keypair)
}

/// Convenience function to create a searcher client with custom endpoint
pub fn create_searcher_client_with_endpoint(
    keypair: Arc<solana_keypair::Keypair>,
    endpoint: String,
) -> Result<SearcherClient, SearcherClientError> {
    let config = SearcherClientConfig {
        endpoint,
        ..SearcherClientConfig::default()
    };
    SearcherClient::new_with_config(keypair, config)
}