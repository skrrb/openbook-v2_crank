use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Market {
    pub market_pk: String,
    pub event_heap: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub program_id: String,
    pub markets: Vec<Market>,
}
