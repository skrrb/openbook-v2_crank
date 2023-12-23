use clap::Parser;
use solana_sdk::pubkey::Pubkey;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    #[arg(short, long, default_value_t = String::from("http://127.0.0.1:8899"))]
    pub rpc_url: String,

    #[arg(short, long, default_value_t = String::from("ws://127.0.0.1:8900"))]
    pub ws_url: String,

    /// tpu fanout
    #[arg(short = 'f', long, default_value_t = 16)]
    pub fanout_size: u64,

    #[arg(short = 'k', long)]
    pub identity: Option<String>,

    #[arg(long, default_value_t = 60)]
    pub duration_in_seconds: u64,

    #[arg(short = 't', long)]
    pub transaction_save_file: Option<String>,

    #[arg(short = 'b', long)]
    pub block_data_save_file: Option<String>,

    #[arg(short = 'a', long, required = true)]
    pub crank_authority: String,

    #[arg(long, default_value_t = 10)]
    pub transaction_retry_in_ms: u64,

    #[arg(long, default_value_t = openbook_v2::ID)]
    pub program_id: Pubkey,

    /// List of markets to crank
    #[arg(long, required = true, num_args = 1..)]
    pub markets: Vec<Pubkey>,
}
