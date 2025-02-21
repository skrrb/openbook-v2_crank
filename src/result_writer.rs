use crate::states::{BlockData, TransactionConfirmRecord};
use async_std::fs::File;
use tokio::{sync::broadcast::Receiver, task::JoinHandle};

pub fn initialize_result_writers(
    transaction_save_file: Option<String>,
    block_data_save_file: Option<String>,
    tx_data: Receiver<TransactionConfirmRecord>,
    block_data: Receiver<BlockData>,
) -> Vec<JoinHandle<()>> {
    let mut tasks = vec![];

    if let Some(transaction_save_file) = transaction_save_file {
        let tx_data_jh = tokio::spawn(async move {
            let mut writer = csv_async::AsyncSerializer::from_writer(
                File::create(transaction_save_file).await.unwrap(),
            );
            let mut tx_data = tx_data;
            while let Ok(record) = tx_data.recv().await {
                writer.serialize(record).await.unwrap();
            }
            writer.flush().await.unwrap();
        });
        tasks.push(tx_data_jh);
    }

    if let Some(block_data_save_file) = block_data_save_file {
        let block_data_jh = tokio::spawn(async move {
            let mut writer = csv_async::AsyncSerializer::from_writer(
                File::create(block_data_save_file).await.unwrap(),
            );
            let mut block_data = block_data;
            while let Ok(record) = block_data.recv().await {
                writer.serialize(record).await.unwrap();
            }
            writer.flush().await.unwrap();
        });
        tasks.push(block_data_jh);
    }
    tasks
}
