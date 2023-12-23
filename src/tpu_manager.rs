use crate::states::TransactionSendRecord;
use crate::stats::CrankStats;
use bincode::serialize;
use log::{error, info, warn};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::{connection_cache::ConnectionCache, nonblocking::tpu_client::TpuClient};
use solana_quic_client::{QuicConfig, QuicConnectionManager, QuicPool};
use solana_sdk::signature::Keypair;
use solana_sdk::transaction::Transaction;
use std::time::Duration;
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};
use tokio::sync::{mpsc::UnboundedSender, RwLock};

pub type QuicTpuClient = TpuClient<QuicPool, QuicConnectionManager, QuicConfig>;

#[derive(Clone)]
pub struct TpuManager {
    error_count: Arc<AtomicU32>,
    rpc_client: Arc<RpcClient>,
    // why arc twice / one is so that we clone rwlock and other so that we can clone tpu client
    tpu_client: Arc<RwLock<Arc<QuicTpuClient>>>,
    pub ws_addr: String,
    fanout_slots: u64,
    identity: Arc<Keypair>,
    tx_send_record: UnboundedSender<TransactionSendRecord>,
    stats: CrankStats,
}

impl TpuManager {
    pub async fn new(
        rpc_client: Arc<RpcClient>,
        ws_addr: String,
        fanout_slots: u64,
        identity: Keypair,
        tx_send_record: UnboundedSender<TransactionSendRecord>,
        stats: CrankStats,
    ) -> Self {
        let connection_cache = ConnectionCache::new_with_client_options(
            "",
            4,
            None,
            Some((&identity, IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)))),
            None,
        );

        let quic_connection_cache =
            if let ConnectionCache::Quic(connection_cache) = connection_cache {
                Some(connection_cache)
            } else {
                None
            };

        let tpu_client = Arc::new(
            TpuClient::new_with_connection_cache(
                rpc_client.clone(),
                &ws_addr,
                solana_client::tpu_client::TpuClientConfig { fanout_slots },
                quic_connection_cache.unwrap(),
            )
            .await
            .unwrap(),
        );

        Self {
            rpc_client,
            tpu_client: Arc::new(RwLock::new(tpu_client)),
            ws_addr,
            fanout_slots,
            error_count: Default::default(),
            identity: Arc::new(identity),
            tx_send_record,
            stats,
        }
    }

    pub async fn reset_tpu_client(&self) -> anyhow::Result<()> {
        let identity = Keypair::from_bytes(&self.identity.to_bytes()).unwrap();
        let connection_cache = ConnectionCache::new_with_client_options(
            "",
            4,
            None,
            Some((&identity, IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)))),
            None,
        );

        let quic_connection_cache =
            if let ConnectionCache::Quic(connection_cache) = connection_cache {
                Some(connection_cache)
            } else {
                None
            };

        let tpu_client = Arc::new(
            TpuClient::new_with_connection_cache(
                self.rpc_client.clone(),
                &self.ws_addr,
                solana_client::tpu_client::TpuClientConfig {
                    fanout_slots: self.fanout_slots,
                },
                quic_connection_cache.unwrap(),
            )
            .await
            .unwrap(),
        );
        self.error_count.store(0, Ordering::Relaxed);
        *self.tpu_client.write().await = tpu_client;
        Ok(())
    }

    pub async fn reset(&self) -> anyhow::Result<()> {
        self.error_count.fetch_add(1, Ordering::Relaxed);

        if self.error_count.load(Ordering::Relaxed) > 5 {
            self.reset_tpu_client().await?;
            info!("TPU Reset after 5 errors");
        }

        Ok(())
    }

    pub fn force_reset_after_every(&self, duration: Duration) {
        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            if let Err(e) = this.reset_tpu_client().await {
                error!("timely restart of tpu client failed {}", e);
            }
        });
    }

    async fn get_tpu_client(&self) -> Arc<QuicTpuClient> {
        self.tpu_client.read().await.clone()
    }

    pub async fn send_transaction_batch(
        &self,
        batch: &Vec<(Transaction, TransactionSendRecord)>,
    ) -> bool {
        let tpu_client = self.get_tpu_client().await;

        for (_tx, record) in batch {
            let tx_sent_record = self.tx_send_record.clone();
            let sent = tx_sent_record.send(record.clone());
            if sent.is_err() {
                warn!(
                    "sending error on channel : {}",
                    sent.err().unwrap().to_string()
                );
            }
        }

        for _ in batch {
            self.stats.inc_send()
        }

        if tpu_client
            .try_send_wire_transaction_batch(
                batch
                    .iter()
                    .map(|(tx, _)| serialize(tx).expect("serialization should succeed"))
                    .collect(),
            )
            .await
            .is_err()
        {
            if let Err(e) = self.reset().await {
                error!("error while reseting tpu client {}", e);
            }
            false
        } else {
            true
        }
    }
}
