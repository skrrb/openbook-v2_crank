use anchor_lang::AccountDeserialize;
use clap::Parser;
use cli::Args;
use confirmation_strategy::confirmations_by_blocks;
use helpers::{
    create_rpc_transaction_bridge, create_tpu_transaction_bridge, start_blockhash_polling_service,
};
use markets::MarketData;
use openbook_v2::state::Market;
use result_writer::initialize_result_writers;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{commitment_config::CommitmentConfig, signature::Keypair};
use stats::CrankStats;
use std::{
    sync::{atomic::AtomicU64, Arc},
    time::Duration,
};
use tokio::sync::{mpsc::unbounded_channel, RwLock};

mod cli;
mod confirmation_strategy;
mod crank;
mod helpers;
mod markets;
mod openbook_v2_sink;
mod result_writer;
mod rpc_manager;
mod states;
mod stats;
mod tpu_manager;

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let crank_authority = {
        let identity_file = tokio::fs::read_to_string(args.crank_authority.as_str())
            .await
            .expect("Cannot find the keeper identity file provided");
        let identity_bytes: Vec<u8> =
            serde_json::from_str(&identity_file).expect("Keypair file invalid");
        Keypair::from_bytes(identity_bytes.as_slice()).expect("Keypair file invalid")
    };

    let rpc_client = Arc::new(RpcClient::new_with_commitment(
        args.rpc_url.to_string(),
        CommitmentConfig::finalized(),
    ));

    let infos = rpc_client
        .get_multiple_accounts(&args.markets)
        .await
        .expect("cannot fetch markets");

    let markets = args
        .markets
        .iter()
        .zip(infos)
        .filter_map(|(pubkey, info)| {
            if let Some(info) = info {
                let market = Market::try_deserialize(&mut &info.data[..])
                    .expect("cannot deserialize market");
                Some(MarketData {
                    market_pk: *pubkey,
                    event_heap: market.event_heap,
                    admin: market.consume_events_admin.into(),
                })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    // create a task that updates blockhash after every interval
    let recent_blockhash = rpc_client
        .get_latest_blockhash()
        .await
        .expect("Rpc URL is not working");
    let last_slot = rpc_client.get_slot().await.expect("Rpc URL is not working");
    let blockhash_rw = Arc::new(RwLock::new(recent_blockhash));
    let current_slot = Arc::new(AtomicU64::new(last_slot));
    let bh_polling_task = start_blockhash_polling_service(
        blockhash_rw.clone(),
        current_slot.clone(),
        rpc_client.clone(),
    );

    let crank_stats = CrankStats::new();
    let (tx_sx, tx_rx) = unbounded_channel();
    let (tx_send_record_sx, tx_send_record_rx) = unbounded_channel();

    // start transaction send bridge either over TPU or RPC
    let transaction_send_bridge_task = if let Some(identitiy_path) = args.identity {
        let identity_file = tokio::fs::read_to_string(identitiy_path.as_str())
            .await
            .expect("Cannot find the identity file provided");
        let identity_bytes: Vec<u8> =
            serde_json::from_str(&identity_file).expect("Keypair file invalid");

        let identity =
            Keypair::from_bytes(identity_bytes.as_slice()).expect("Keypair file invalid");

        let tpu_manager = Arc::new(
            tpu_manager::TpuManager::new(
                rpc_client.clone(),
                args.ws_url.clone(),
                16,
                identity,
                tx_send_record_sx,
                crank_stats.clone(),
            )
            .await,
        );
        tpu_manager.force_reset_after_every(Duration::from_secs(600)); // reset every 10 minutes
        create_tpu_transaction_bridge(tx_rx, tpu_manager, 16, Duration::from_millis(5))
    } else {
        let rpc_manager = Arc::new(rpc_manager::RpcManager::new(
            rpc_client.clone(),
            tx_send_record_sx,
            crank_stats.clone(),
        ));
        create_rpc_transaction_bridge(tx_rx, rpc_manager, Duration::from_millis(5))
    };

    // start event queue crank
    let mut crank_services = crank::start(
        crank::KeeperConfig {
            program_id: args.program_id,
            rpc_url: args.rpc_url.to_string(),
            websocket_url: args.ws_url.to_string(),
        },
        blockhash_rw.clone(),
        current_slot.clone(),
        &markets,
        &crank_authority,
        1000,
        tx_sx.clone(),
    );

    // start confirmations by blocks
    let (tx_confirmation_sx, tx_confirmation_rx) = tokio::sync::broadcast::channel(8192);
    let (blocks_confirmation_sx, blocks_confirmation_rx) = tokio::sync::broadcast::channel(8192);

    crank_stats.update_from_tx_status_stream(tx_confirmation_sx.subscribe());
    let mut confirmation_services = confirmations_by_blocks(
        rpc_client.clone(),
        tx_send_record_rx,
        tx_confirmation_sx,
        blocks_confirmation_sx,
        current_slot.load(std::sync::atomic::Ordering::Relaxed),
    );

    // start writing results
    initialize_result_writers(
        args.transaction_save_file.clone(),
        args.block_data_save_file.clone(),
        tx_confirmation_rx,
        blocks_confirmation_rx,
    );

    // task which updates stats
    let mut stats = crank_stats.clone();
    let reporting_thread = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            stats.report().await;
        }
    });

    crank_services.append(&mut confirmation_services);
    crank_services.push(bh_polling_task);
    crank_services.push(transaction_send_bridge_task);
    crank_services.push(reporting_thread);

    let _ = futures::future::select_all(crank_services).await;

    Ok(())
}
