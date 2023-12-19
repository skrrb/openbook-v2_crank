// simlar to json config just the Keypairs are changed to Pubkeys

use crate::json_config::{Config, Market};
use itertools::Itertools;
use solana_program::pubkey::Pubkey;

#[derive(Clone, Debug)]
pub struct Obv2Market {
    pub market_pk: Pubkey,
    pub event_heap: Pubkey,
}

#[derive(Clone, Debug)]
pub struct Obv2Config {
    pub program_id: Pubkey,
    pub markets: Vec<Obv2Market>,
}

pub fn convert_to_pk(key: &String) -> Pubkey {
    Pubkey::try_from(key.as_str()).expect("Should be convertible to pubkey")
}

impl From<&Config> for Obv2Config {
    fn from(value: &Config) -> Self {
        Self {
            program_id: convert_to_pk(&value.program_id),
            markets: value
                .markets
                .iter()
                .map(|x| Obv2Market::try_from(x).expect("Market should be Pubkey"))
                .collect_vec(),
        }
    }
}

impl From<&Market> for Obv2Market {
    fn from(value: &Market) -> Self {
        Self {
            market_pk: convert_to_pk(&value.market_pk),
            event_heap: convert_to_pk(&value.event_heap),
        }
    }
}
