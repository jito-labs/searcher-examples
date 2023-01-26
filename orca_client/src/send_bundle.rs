use std::{str::FromStr, time::Duration};

use futures::StreamExt;
use jito_protos::{
    bundle::Bundle,
    convert::proto_packet_from_versioned_tx,
    searcher::{
        searcher_service_client::SearcherServiceClient, NextScheduledLeaderRequest,
        SendBundleRequest, SubscribeBundleResultsRequest,
    },
};
use jito_searcher_client::token_authenticator::ClientInterceptor;

use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{read_keypair_file, Signature, Signer},
    system_instruction::transfer,
    transaction::{Transaction, VersionedTransaction},
};
use tokio::time::{sleep, timeout};
use tonic::{codegen::InterceptedService, transport::Channel};

type Client = SearcherServiceClient<InterceptedService<Channel, ClientInterceptor>>;

/// Builds and send bundle, attaching the tip and waiting until the next leader
pub async fn build_and_send_bundle(
    rpc_url: String,
    bundle: Vec<VersionedTransaction>,
    keypair_path: String,
    tip_lamports: u64,
    tip_account: String,
    client: &mut Client,
) {
    let payer_keypair = read_keypair_file(keypair_path).expect("reads keypair at path");
    let tip_account = Pubkey::from_str(&tip_account).expect("valid pubkey for tip account");
    let rpc_client = RpcClient::new(rpc_url);
    let balance = rpc_client
        .get_balance(&payer_keypair.pubkey())
        .await
        .expect("reads balance");

    println!(
        "payer public key: {:?} lamports: {:?}",
        payer_keypair.pubkey(),
        balance
    );

    let mut bundle_results_subscription = client
        .subscribe_bundle_results(SubscribeBundleResultsRequest {})
        .await
        .expect("subscribe to bundle results")
        .into_inner();

    // wait for jito-solana leader slot
    let mut is_leader_slot = false;
    while !is_leader_slot {
        let next_leader = client
            .get_next_scheduled_leader(NextScheduledLeaderRequest {})
            .await
            .expect("gets next scheduled leader")
            .into_inner();
        let num_slots = next_leader.next_leader_slot - next_leader.current_slot;
        is_leader_slot = num_slots <= 2;
        println!("next jito leader slot in {} slots", num_slots);
        sleep(Duration::from_millis(500)).await;
    }

    // build + sign the transactions
    let blockhash = rpc_client
        .get_latest_blockhash()
        .await
        .expect("get blockhash");

    let tip_tx = VersionedTransaction::from(Transaction::new_signed_with_payer(
        &[transfer(
            &payer_keypair.pubkey(),
            &tip_account,
            tip_lamports,
        )],
        Some(&payer_keypair.pubkey()),
        &[&payer_keypair],
        blockhash.clone(),
    ));
    let txs = [bundle, vec![tip_tx]].concat();

    let signatures: Vec<Signature> = txs.iter().map(|tx| tx.signatures[0]).collect();

    // convert them to packets + send over
    let packets: Vec<_> = txs.iter().map(proto_packet_from_versioned_tx).collect();
    let result = client
        .send_bundle(SendBundleRequest {
            bundle: Some(Bundle {
                header: None,
                packets,
            }),
        })
        .await
        .expect("sends bundle");

    // grab uuid from block engine + wait for results
    let uuid = result.into_inner().uuid;
    println!("bundle sent uuid: {:?}", uuid);
    println!("waiting for 5 seconds to hear results...");
    while let Ok(Some(Ok(results))) =
        timeout(Duration::from_secs(5), bundle_results_subscription.next()).await
    {
        println!("bundle results: {:?}", results);
    }

    let futs: Vec<_> = signatures
        .iter()
        .map(|sig| {
            rpc_client.get_signature_status_with_commitment(sig, CommitmentConfig::processed())
        })
        .collect();
    let results = futures::future::join_all(futs).await;
    if !results.iter().all(|r| matches!(r, Ok(Some(Ok(()))))) {
        println!("transactions in bundle did not land :(");
    } else {
        println!("bundle landed successfully!!");
        for sig in signatures {
            println!("https://solscan.io/tx/{}", sig);
        }
    }
}
