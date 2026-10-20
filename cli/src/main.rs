use std::{env, path::PathBuf, sync::Arc};

use clap::{Parser, Subcommand};
use env_logger::TimestampPrecision;
use jito_searcher_client::{SearcherClient, SearcherClientConfig};
use log::{info, error};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_keypair::{Keypair, read_keypair_file};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_instruction::{AccountMeta, Instruction};
#[allow(deprecated)]
use solana_program::system_instruction;
use solana_transaction::Transaction;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// URL of the block engine.
    /// See: https://jito-labs.gitbook.io/mev/searcher-resources/block-engine#connection-details
    #[arg(long, env)]
    block_engine_url: String,

    /// Path to keypair file used to authenticate with the Jito Block Engine
    /// See: https://jito-labs.gitbook.io/mev/searcher-resources/getting-started#block-engine-api-key
    #[arg(long, env)]
    keypair_path: Option<PathBuf>,

    /// Subcommand to run
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Prints out information on connected leaders
    ConnectedLeaders,

    /// Prints out connected leaders with their leader slot percentage
    ConnectedLeadersInfo {
        #[clap(long, required = true)]
        rpc_url: String,
    },

    /// Prints out information about the tip accounts
    TipAccounts,

    /// Sends a bundle to the block engine
    SendBundle {
        /// RPC URL
        #[clap(long, required = true)]
        rpc_url: String,
        /// Filepath to keypair that can afford the transaction payments with tip
        #[clap(long, required = true)]
        payer: PathBuf,
        /// Message you'd like the bundle to say
        #[clap(long, required = true)]
        message: String,
        /// Number of transactions in the bundle (must be <= 5)
        #[clap(long, required = true)]
        num_txs: usize,
        /// Amount of lamports to tip in each transaction
        #[clap(long, required = true)]
        lamports: u64,
        /// One of the tip accounts, see https://jito-foundation.gitbook.io/mev/mev-payment-and-distribution/on-chain-addresses
        #[clap(long, required = true)]
        tip_account: Pubkey,
    },
}

async fn create_searcher_client(block_engine_url: &str, keypair: Option<Arc<Keypair>>) -> Result<SearcherClient, Box<dyn std::error::Error>> {
    let config = SearcherClientConfig {
        endpoint: block_engine_url.to_string(),
        ..SearcherClientConfig::default()
    };
    
    // Always create client but don't connect/authenticate yet
    match keypair {
        Some(kp) => {
            info!("Creating authenticated searcher client...");
            let client = SearcherClient::new_with_config(kp, config)?;
            // Don't call connect() here - let the individual methods handle auth
            Ok(client)
        }
        None => {
            info!("Creating non-authenticated searcher client...");
            // Create with dummy keypair but won't authenticate
            let dummy_keypair = Arc::new(Keypair::new());
            let client = SearcherClient::new_with_config(dummy_keypair, config)?;
            Ok(client)
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Args = Args::parse();
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info")
    }
    env_logger::builder()
        .format_timestamp(Some(TimestampPrecision::Micros))
        .init();

    let keypair = args
        .keypair_path
        .as_ref()
        .map(|path| Arc::new(read_keypair_file(path).expect("parse kp file")));

    let client = create_searcher_client(&args.block_engine_url, keypair.clone())
        .await
        .expect("Failed to create searcher client");

    process_commands(args, client).await;
}

async fn process_commands(args: Args, client: SearcherClient) {
    match args.command {
        Commands::ConnectedLeaders => {
            match client.get_connected_leaders().await {
                Ok(connected_leaders) => {
                    info!("Connected leaders: {:#?}", connected_leaders);
                }
                Err(e) => {
                    eprintln!("Error getting connected leaders: {}", e);
                }
            }
        }
        Commands::ConnectedLeadersInfo { rpc_url } => {
            match client.get_connected_leaders().await {
                Ok(connected_validators) => {
                    let rpc_client = RpcClient::new(rpc_url);
                    match rpc_client.get_vote_accounts().await {
                        Ok(rpc_vote_account_status) => {
                            let total_activated_stake: u64 = rpc_vote_account_status
                                .current
                                .iter()
                                .chain(rpc_vote_account_status.delinquent.iter())
                                .map(|vote_account| vote_account.activated_stake)
                                .sum();

                            let mut total_activated_connected_stake = 0;
                            for rpc_vote_account_info in rpc_vote_account_status.current {
                                if connected_validators.contains_key(&rpc_vote_account_info.node_pubkey) {
                                    total_activated_connected_stake += rpc_vote_account_info.activated_stake;
                                    info!(
                                        "connected_leader: {}, stake: {:.2}%",
                                        rpc_vote_account_info.node_pubkey,
                                        (rpc_vote_account_info.activated_stake * 100) as f64
                                            / total_activated_stake as f64
                                    );
                                }
                            }
                            info!(
                                "total stake for block engine: {:.2}%",
                                (total_activated_connected_stake * 100) as f64 / total_activated_stake as f64
                            );
                        }
                        Err(e) => {
                            eprintln!("Error getting vote accounts: {}", e);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error getting connected leaders: {}", e);
                }
            }
        }
        Commands::TipAccounts => {
            info!("Getting tip accounts (no authentication required)...");
            
            match client.get_tip_accounts_no_auth().await {
                Ok(accounts) => {
                    info!("SUCCESS! Retrieved {} tip accounts:", accounts.len());
                    for (i, account) in accounts.iter().enumerate() {
                        println!("Tip Account {}: {}", i + 1, account);
                    }
                }
                Err(e) => {
                    error!("Failed to get tip accounts: {}", e);
                    eprintln!("Error: {}", e);
                }
            }
        }
        Commands::SendBundle {
            rpc_url,
            payer,
            message,
            num_txs,
            lamports,
            tip_account,
        } => {
            let payer_keypair = read_keypair_file(&payer).expect("reads keypair at path");
            let rpc_client = RpcClient::new(rpc_url);
            
            match rpc_client.get_balance(&payer_keypair.pubkey()).await {
                Ok(balance) => {
                    info!(
                        "payer public key: {:?} lamports: {balance:?}",
                        payer_keypair.pubkey(),
                    );
                }
                Err(e) => {
                    eprintln!("Error getting balance: {}", e);
                    return;
                }
            }

            // Build + sign the transactions
            match rpc_client.get_latest_blockhash().await {
                Ok(blockhash) => {
                    let txs: Vec<_> = (0..num_txs)
                        .map(|i| {
                            let memo_ix = Instruction::new_with_bytes(
                                spl_memo::id(),
                                format!("jito bundle {i}: {message}").as_bytes(),
                                vec![AccountMeta::new(payer_keypair.pubkey(), true)],
                            );
                            
                            let transfer_ix = system_instruction::transfer(
                                &payer_keypair.pubkey(),
                                &tip_account,
                                lamports,
                            );

                            Transaction::new_signed_with_payer(
                                &[memo_ix, transfer_ix],
                                Some(&payer_keypair.pubkey()),
                                &[&payer_keypair],
                                blockhash,
                            )
                        })
                        .collect();

                    // Send bundle - use auth or no-auth based on keypair availability
                    let result = if args.keypair_path.is_some() {
                        info!("Sending bundle with authentication...");
                        client.send_bundle_with_transactions(txs).await
                    } else {
                        info!("Sending bundle without authentication...");
                        client.send_bundle_with_transactions_no_auth(txs).await
                    };

                    match result {
                        Ok(bundle_id) => {
                            info!("Bundle sent successfully! UUID: {}", bundle_id);
                        }
                        Err(e) => {
                            eprintln!("Error sending bundle: {}", e);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error getting latest blockhash: {}", e);
                }
            }
        }
    }
}