use crate::states::{BlockData, TransactionConfirmRecord, TransactionSendRecord};
use chrono::Utc;
use dashmap::DashMap;
use log::{debug, warn};
use solana_client::{nonblocking::rpc_client::RpcClient, rpc_config::RpcBlockConfig};
use solana_sdk::{
    commitment_config::{CommitmentConfig, CommitmentLevel},
    signature::Signature,
    slot_history::Slot,
};
use solana_transaction_status::{
    RewardType, TransactionDetails, UiConfirmedBlock, UiTransactionEncoding,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::broadcast::Sender, sync::mpsc::UnboundedReceiver, task::JoinHandle, time::Instant,
};

pub async fn process_blocks(
    block: &UiConfirmedBlock,
    tx_confirm_records: Sender<TransactionConfirmRecord>,
    tx_block_data: Sender<BlockData>,
    transaction_map: Arc<DashMap<Signature, (TransactionSendRecord, Instant)>>,
    slot: u64,
) {
    let mut mm_transaction_count: u64 = 0;
    let rewards = block.rewards.as_ref().unwrap();
    let slot_leader = match rewards
        .iter()
        .find(|r| r.reward_type == Some(RewardType::Fee))
    {
        Some(x) => x.pubkey.clone(),
        None => "".to_string(),
    };

    if let Some(transactions) = &block.transactions {
        let nb_transactions = transactions.len();
        let mut cu_consumed: u64 = 0;
        let mut cu_consumed_by_obv2: u64 = 0;
        for solana_transaction_status::EncodedTransactionWithStatusMeta {
            transaction, meta, ..
        } in transactions
        {
            let transaction = match transaction.decode() {
                Some(tx) => tx,
                None => {
                    continue;
                }
            };
            for signature in &transaction.signatures {
                let transaction_record_op = {
                    let rec = transaction_map.get(signature);
                    rec.map(|x| x.clone())
                };
                // add CU in counter
                let tx_cu = if let Some(meta) = &meta {
                    match meta.compute_units_consumed {
                        solana_transaction_status::option_serializer::OptionSerializer::Some(x) => {
                            cu_consumed = cu_consumed.saturating_add(x);
                            x
                        }
                        _ => 0,
                    }
                } else {
                    0
                };

                if let Some(transaction_record) = transaction_record_op {
                    let transaction_record = transaction_record.0;
                    mm_transaction_count += 1;
                    cu_consumed_by_obv2 += tx_cu;

                    match tx_confirm_records.send(TransactionConfirmRecord {
                        signature: transaction_record.signature.to_string(),
                        confirmed_slot: Some(slot),
                        confirmed_at: Some(Utc::now().to_string()),
                        sent_at: transaction_record.sent_at.to_string(),
                        sent_slot: transaction_record.sent_slot,
                        successful: if let Some(meta) = &meta {
                            meta.status.is_ok()
                        } else {
                            false
                        },
                        error: if let Some(meta) = &meta {
                            meta.err.as_ref().map(|x| x.to_string())
                        } else {
                            None
                        },
                        block_hash: Some(block.blockhash.clone()),
                        market: transaction_record.market.map(|x| x.to_string()),
                        user: transaction_record.user.map(|x| x.to_string()),
                        slot_processed: Some(slot),
                        slot_leader: Some(slot_leader.clone()),
                        timed_out: false,
                        priority_fees: transaction_record.priority_fees,
                    }) {
                        Ok(_) => {}
                        Err(e) => {
                            warn!("Tx confirm record channel broken {}", e.to_string());
                        }
                    }
                }

                transaction_map.remove(signature);
            }
        }

        // push block data
        {
            let filled_percentage = (cu_consumed_by_obv2 * 100) as f32 / cu_consumed as f32;
            let _ = tx_block_data.send(BlockData {
                block_hash: block.blockhash.clone(),
                block_leader: slot_leader,
                block_slot: slot,
                block_time: if let Some(time) = block.block_time {
                    time as u64
                } else {
                    0
                },
                number_of_mm_transactions: mm_transaction_count,
                total_transactions: nb_transactions as u64,
                cu_consumed,
                percentage_filled_by_openbook: filled_percentage,
            });
        }
    }
}

async fn get_blocks_with_retry(
    client: Arc<RpcClient>,
    start_block: u64,
    commitment_confirmation: CommitmentConfig,
) -> Result<Vec<Slot>, ()> {
    const N_TRY_REQUEST_BLOCKS: u64 = 4;
    for _ in 0..N_TRY_REQUEST_BLOCKS {
        let block_slots = client
            .get_blocks_with_commitment(start_block, None, commitment_confirmation)
            .await;

        match block_slots {
            Ok(slots) => {
                return Ok(slots);
            }
            Err(error) => {
                warn!("Failed to download blocks: {}, retry", error);
            }
        }
    }
    Err(())
}

pub fn confirmations_by_blocks(
    client: Arc<RpcClient>,
    mut tx_record_rx: UnboundedReceiver<TransactionSendRecord>,
    tx_confirm_records: tokio::sync::broadcast::Sender<TransactionConfirmRecord>,
    tx_block_data: tokio::sync::broadcast::Sender<BlockData>,
    from_slot: u64,
) -> Vec<JoinHandle<()>> {
    let transaction_map = Arc::new(DashMap::new());

    let map_filler_jh = {
        let transaction_map = transaction_map.clone();
        tokio::spawn(async move {
            loop {
                if let Ok(tx_record) =
                    tokio::time::timeout(tokio::time::Duration::from_secs(1), tx_record_rx.recv())
                        .await
                {
                    match tx_record {
                        Some(tx_record) => {
                            debug!(
                                "add to queue len={} sig={}",
                                transaction_map.len() + 1,
                                tx_record.signature
                            );
                            transaction_map
                                .insert(tx_record.signature, (tx_record, Instant::now()));
                        }
                        None => {
                            break;
                        }
                    }
                }
            }
        })
    };

    let cleaner_jh = {
        let transaction_map = transaction_map.clone();
        let tx_confirm_records = tx_confirm_records.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                {
                    let mut to_remove = vec![];

                    for tx_data in transaction_map.iter() {
                        let sent_record = &tx_data.0;
                        let instant = tx_data.1;
                        let signature = tx_data.key();
                        let remove = instant.elapsed() > Duration::from_secs(120);

                        // add to timeout if not retaining
                        if remove {
                            let _ = tx_confirm_records.send(TransactionConfirmRecord {
                                signature: signature.to_string(),
                                confirmed_slot: None,
                                confirmed_at: None,
                                sent_at: sent_record.sent_at.to_string(),
                                sent_slot: sent_record.sent_slot,
                                successful: false,
                                error: Some("timeout".to_string()),
                                block_hash: None,
                                market: sent_record.market.map(|x| x.to_string()),
                                user: sent_record.user.map(|x| x.to_string()),
                                slot_processed: None,
                                slot_leader: None,
                                timed_out: true,
                                priority_fees: sent_record.priority_fees,
                            });
                            to_remove.push(*signature);
                        }
                    }

                    for signature in to_remove {
                        transaction_map.remove(&signature);
                    }
                }
            }
        })
    };

    let block_confirmation_jh = {
        tokio::spawn(async move {
            let mut start_block = from_slot;
            let mut start_instant = tokio::time::Instant::now();
            let refresh_in = Duration::from_secs(10);
            let commitment_confirmation = CommitmentConfig {
                commitment: CommitmentLevel::Confirmed,
            };
            loop {
                let wait_duration = tokio::time::Instant::now() - start_instant;
                if wait_duration < refresh_in {
                    tokio::time::sleep(refresh_in - wait_duration).await;
                }
                start_instant = tokio::time::Instant::now();

                let block_slots =
                    get_blocks_with_retry(client.clone(), start_block, commitment_confirmation)
                        .await;
                if block_slots.is_err() {
                    break;
                }

                let block_slots = block_slots.unwrap();
                if block_slots.is_empty() {
                    continue;
                }
                start_block = *block_slots.last().unwrap() + 1;

                let blocks = block_slots.iter().map(|slot| {
                    client.get_block_with_config(
                        *slot,
                        RpcBlockConfig {
                            encoding: Some(UiTransactionEncoding::Base64),
                            transaction_details: Some(TransactionDetails::Full),
                            rewards: Some(true),
                            commitment: Some(commitment_confirmation),
                            max_supported_transaction_version: Some(0),
                        },
                    )
                });
                let blocks = futures::future::join_all(blocks).await;
                for block_slot in blocks.iter().zip(block_slots) {
                    let block = match block_slot.0 {
                        Ok(x) => x,
                        Err(_) => continue,
                    };
                    let tx_confirm_records = tx_confirm_records.clone();
                    let tx_block_data = tx_block_data.clone();
                    let transaction_map = transaction_map.clone();
                    process_blocks(
                        block,
                        tx_confirm_records,
                        tx_block_data,
                        transaction_map,
                        block_slot.1,
                    )
                    .await;
                }
            }
        })
    };
    vec![map_filler_jh, cleaner_jh, block_confirmation_jh]
}
