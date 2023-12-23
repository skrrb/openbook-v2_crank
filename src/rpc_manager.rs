use crate::states::TransactionSendRecord;
use crate::stats::CrankStats;
use log::{error, warn};
use solana_client::nonblocking::rpc_client::RpcClient;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone)]
pub struct RpcManager {
    rpc_client: Arc<RpcClient>,
    tx_send_record: UnboundedSender<TransactionSendRecord>,
    stats: CrankStats,
}

impl RpcManager {
    pub fn new(
        rpc_client: Arc<RpcClient>,
        tx_send_record: UnboundedSender<TransactionSendRecord>,
        stats: CrankStats,
    ) -> Self {
        Self {
            rpc_client,
            tx_send_record,
            stats,
        }
    }

    pub async fn send_transaction(
        &self,
        transaction: &solana_sdk::transaction::Transaction,
        transaction_sent_record: TransactionSendRecord,
    ) -> bool {
        self.stats.inc_send();

        let tx_sent_record = self.tx_send_record.clone();
        let sent = tx_sent_record.send(transaction_sent_record);
        if sent.is_err() {
            warn!(
                "sending error on channel : {}",
                sent.err().unwrap().to_string()
            );
        }

        let res = self.rpc_client.send_transaction(transaction).await;
        if let Err(e) = &res {
            error!("error sending txs over rpc {}", e);
        }
        res.is_ok()
    }
}
