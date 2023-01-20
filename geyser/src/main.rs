use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use geyser_grpc_plugin_client::geyser_proto::{
    geyser_client::GeyserClient, maybe_account_update::Msg, EmptyRequest, MaybeAccountUpdate,
    SlotUpdateStatus, SubscribeAccountUpdatesRequest, SubscribeProgramsUpdatesRequest,
};
use prost_types::Timestamp;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tonic::{
    service::Interceptor,
    transport::{ClientTlsConfig, Endpoint},
    Status, Streaming,
};
use uuid::Uuid;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// URL without the protocol header (https, http, grpc, ws, etc.) and without the access token
    #[clap(long, env, default_value = "https://mainnet.rpc.jito.wtf")]
    url: String,

    /// access token uuid
    #[clap(long, env)]
    access_token: Uuid,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Subscribe to slot updates from Geyser
    Slots,

    /// Similar to Solana's programSubscribe, it will send you updates when an account owned by any of
    /// the programs have a change in state
    Programs {
        /// A space-separated list of programs to subscribe to
        #[clap(required = true)]
        programs: Vec<String>,
    },

    /// Subscribe to a set of accounts
    Accounts {
        /// A space-separated list of accounts to subscribe to
        #[clap(required = true)]
        accounts: Vec<String>,
    },
}

struct GrpcInterceptor {
    access_token: Uuid,
}

impl Interceptor for GrpcInterceptor {
    fn call(&mut self, mut request: tonic::Request<()>) -> Result<tonic::Request<()>, Status> {
        request.metadata_mut().insert(
            "access-token",
            self.access_token.to_string().parse().unwrap(),
        );
        Ok(request)
    }
}

#[tokio::main]
async fn main() {
    let args: Args = Args::parse();
    println!("args: {:?}", args);

    // The geyser client must use https and ensure their OS and client is configured for TLS
    let url = format!("https://{}", args.url);
    let channel = Endpoint::from_str(&url)
        .expect("valid url")
        .tls_config(ClientTlsConfig::new())
        .expect("create tls config")
        .connect()
        .await
        .expect("connects");

    // The access token is provided as "access-token": "{uuid_v4}" in the request header
    let interceptor = GrpcInterceptor {
        access_token: args.access_token,
    };
    let mut client = GeyserClient::with_interceptor(channel, interceptor);

    match args.command {
        None => {}
        Some(Commands::Slots {}) => {
            let mut stream = client
                .subscribe_slot_updates(EmptyRequest {})
                .await
                .expect("subscribes to slot stream")
                .into_inner();
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(update) => {
                        println!(
                            "slot: {} parent: {:?} status: {:?}",
                            update.slot,
                            update.parent_slot,
                            SlotUpdateStatus::from_i32(update.status)
                        );
                    }
                    Err(e) => {
                        println!("subscribe_slot_updates error: {:?}", e);
                    }
                }
            }
        }
        Some(Commands::Programs { programs: accounts }) => {
            println!("subscribing to programs: {:?}", accounts);
            let response = client
                .subscribe_program_updates(SubscribeProgramsUpdatesRequest {
                    programs: accounts
                        .iter()
                        .map(|a| Pubkey::from_str(a).unwrap().to_bytes().to_vec())
                        .collect(),
                })
                .await
                .expect("subscribe to geyser")
                .into_inner();
            print_account_updates(response).await;
        }
        Some(Commands::Accounts { accounts }) => {
            println!("subscribing to programs: {:?}", accounts);
            let response = client
                .subscribe_account_updates(SubscribeAccountUpdatesRequest {
                    accounts: accounts
                        .iter()
                        .map(|a| Pubkey::from_str(a).unwrap().to_bytes().to_vec())
                        .collect(),
                })
                .await
                .expect("subscribe to geyser")
                .into_inner();
            print_account_updates(response).await;
        }
    }
}

// calculates a pseudo latency. assumes clocks are synced
fn calc_latency(ts: &Timestamp) -> Duration {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let packet_ts = Duration::new(ts.seconds as u64, ts.nanos as u32);
    now.checked_sub(packet_ts)
        .unwrap_or_else(|| packet_ts.checked_sub(now).unwrap())
}

async fn print_account_updates(mut response: Streaming<MaybeAccountUpdate>) {
    loop {
        let account_update = response
            .message()
            .await
            .expect("get slot update")
            .expect("get slot update");
        match account_update.msg {
            None => {
                println!("error, exiting...");
                break;
            }
            Some(Msg::AccountUpdate(update)) => {
                let latency = calc_latency(&update.ts.unwrap());
                println!(
                    "geyser response seq: {:?} slot: {:?} pubkey: {:?} skew: {:?}",
                    update.seq,
                    update.slot,
                    Pubkey::new(update.pubkey.as_slice()),
                    latency
                );
            }
            Some(Msg::Hb(_)) => {
                println!("heartbeat");
            }
        }
    }
}
