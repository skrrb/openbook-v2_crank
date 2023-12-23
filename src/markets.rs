use solana_program::pubkey::Pubkey;

#[derive(Clone, Debug)]
pub struct MarketData {
    pub market_pk: Pubkey,
    pub event_heap: Pubkey,
    pub admin: Option<Pubkey>,
}

#[derive(Clone, Debug)]
pub struct Obv2Config {
    pub markets: Vec<MarketData>,
}
