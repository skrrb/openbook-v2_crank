use crate::{
    crank::{AccountData, AccountWriteSink},
    markets::MarketData,
};
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use async_channel::Sender;
use async_trait::async_trait;
use bytemuck::cast_ref;
use openbook_v2::state::{EventHeap, EventType, FillEvent, OutEvent};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use solana_sdk::account::ReadableAccount;
use std::collections::{BTreeMap, HashSet};

const MAX_BACKLOG: usize = 2;
const MAX_EVENTS_PER_TX: usize = 50;
const MAX_ACCS_PER_TX: usize = 24;

pub trait ToAccountMetasWrapper {
    fn to_account_metas_wrapper(&self, program_id: Pubkey) -> Vec<AccountMeta>;
}

impl<T: ToAccountMetas> ToAccountMetasWrapper for T {
    fn to_account_metas_wrapper(&self, program_id: Pubkey) -> Vec<AccountMeta> {
        let mut metas = self.to_account_metas(None);
        metas
            .iter_mut()
            .filter(|meta| meta.pubkey == openbook_v2::ID)
            .for_each(|meta| meta.pubkey = program_id);
        metas
    }
}

pub struct OpenbookV2CrankSink {
    instruction_sender: Sender<(Pubkey, Vec<Instruction>)>,
    map_event_q_to_market: BTreeMap<Pubkey, Pubkey>,
    program_id: Pubkey,
}

impl OpenbookV2CrankSink {
    pub fn new(
        markets: Vec<MarketData>,
        instruction_sender: Sender<(Pubkey, Vec<Instruction>)>,
        program_id: Pubkey,
    ) -> Self {
        let mut map_event_q_to_market = BTreeMap::new();
        for market in &markets {
            map_event_q_to_market.insert(market.event_heap, market.market_pk);
        }
        Self {
            instruction_sender,
            map_event_q_to_market,
            program_id,
        }
    }
}

#[async_trait]
impl AccountWriteSink for OpenbookV2CrankSink {
    async fn process(
        &self,
        pk: &solana_sdk::pubkey::Pubkey,
        account: &AccountData,
    ) -> Result<(), String> {
        let account = &account.account;

        let (ix, mkt_pk): (Result<Instruction, String>, Pubkey) = {
            let mut header_data: &[u8] = account.data();

            let event_heap: EventHeap = EventHeap::try_deserialize(&mut header_data)
                .expect("event queue should be correctly deserailizable");

            // only crank if at least 1 fill or a sufficient events of other categories are buffered
            let contains_fill_events = event_heap
                .iter()
                .any(|e| e.0.event_type == EventType::Fill as u8);
            let len = event_heap.iter().count();
            let has_backlog = len > MAX_BACKLOG;
            let seq_num = event_heap.header.seq_num;
            log::debug!("evq {pk:?} seq_num={seq_num} len={len} contains_fill_events={contains_fill_events} has_backlog={has_backlog}");

            if !contains_fill_events && !has_backlog {
                return Err("throttled".into());
            }

            let mut events_accounts = HashSet::new();
            event_heap.iter().take(MAX_EVENTS_PER_TX).for_each(|e| {
                if events_accounts.len() < MAX_ACCS_PER_TX {
                    match EventType::try_from(e.0.event_type).expect("openbook v2 event") {
                        EventType::Fill => {
                            let fill: &FillEvent = cast_ref(e.0);
                            events_accounts.insert(fill.maker);
                            events_accounts.insert(fill.taker);
                        }
                        EventType::Out => {
                            let out: &OutEvent = cast_ref(e.0);
                            events_accounts.insert(out.owner);
                        }
                    }
                }
            });

            let mkt_pk = self
                .map_event_q_to_market
                .get(pk)
                .unwrap_or_else(|| panic!("{pk:?} is a known public key"));

            let mut accounts_meta = openbook_v2::accounts::ConsumeEvents {
                consume_events_admin: None,
                event_heap: *pk,
                market: *mkt_pk,
            }
            .to_account_metas_wrapper(self.program_id);

            for event_account in events_accounts {
                accounts_meta.push(AccountMeta {
                    pubkey: event_account,
                    is_signer: false,
                    is_writable: true,
                })
            }

            let instruction_data = openbook_v2::instruction::ConsumeEvents {
                limit: MAX_EVENTS_PER_TX,
            };

            let ix = Instruction::new_with_bytes(
                self.program_id,
                instruction_data.data().as_slice(),
                accounts_meta,
            );
            (Ok(ix), *mkt_pk)
        };

        if let Err(e) = self.instruction_sender.send((mkt_pk, vec![ix?])).await {
            return Err(e.to_string());
        }

        Ok(())
    }
}
