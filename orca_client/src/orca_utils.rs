/// Helper functions courtesy of @yugure-orca ( https://github.com/yugure-orca/whirlpools )
use std::cell::RefCell;

use anchor_client::solana_sdk::pubkey::Pubkey;
use whirlpool::{
    manager::swap_manager::swap,
    math::{MAX_SQRT_PRICE_X64, MIN_SQRT_PRICE_X64},
    state::{TickArray, Whirlpool, MAX_TICK_INDEX, MIN_TICK_INDEX, TICK_ARRAY_SIZE},
    util::SwapTickSequence,
};

pub fn div_floor(a: i32, b: i32) -> i32 {
    if a < 0 && a % b != 0 {
        a / b - 1
    } else {
        a / b
    }
}

pub fn tickutil_get_start_tick_index(
    tick_current_index: i32,
    tick_spacing: u16,
    offset: i32,
) -> i32 {
    let ticks_in_array = TICK_ARRAY_SIZE * tick_spacing as i32;
    let real_index = div_floor(tick_current_index, ticks_in_array);
    let start_tick_index = (real_index + offset) * ticks_in_array;

    assert!(MIN_TICK_INDEX <= start_tick_index);
    assert!(start_tick_index + ticks_in_array <= MAX_TICK_INDEX);
    start_tick_index
}

pub fn pdautil_get_tick_array(
    program_id: &Pubkey,
    whirlpool_pubkey: &Pubkey,
    start_tick_index: i32,
) -> Pubkey {
    let start_tick_index_str = start_tick_index.to_string();
    let seeds = [
        b"tick_array",
        whirlpool_pubkey.as_ref(),
        start_tick_index_str.as_bytes(),
    ];
    let (pubkey, _bump) = Pubkey::find_program_address(&seeds, program_id);
    pubkey
}

pub fn poolutil_get_tick_array_pubkeys_for_swap(
    tick_current_index: i32,
    tick_spacing: u16,
    a_to_b: bool,
    program_id: &Pubkey,
    whirlpool_pubkey: &Pubkey,
) -> [Pubkey; 3] {
    let mut offset = 0;
    let mut pubkeys: [Pubkey; 3] = Default::default();
    let shifted = if a_to_b { 0i32 } else { tick_spacing as i32 };

    for i in 0..pubkeys.len() {
        let start_tick_index =
            tickutil_get_start_tick_index(tick_current_index + shifted, tick_spacing, offset);
        let tick_array_pubkey =
            pdautil_get_tick_array(program_id, whirlpool_pubkey, start_tick_index);
        pubkeys[i] = tick_array_pubkey;
        offset = if a_to_b { offset - 1 } else { offset + 1 };
    }

    pubkeys
}

pub fn pdautil_get_oracle(program_id: &Pubkey, whirlpool_pubkey: &Pubkey) -> Pubkey {
    let seeds = [b"oracle", whirlpool_pubkey.as_ref()];
    let (pubkey, _bump) = Pubkey::find_program_address(&seeds, program_id);
    pubkey
}

pub fn get_swap_quote(
    whirlpool: &Whirlpool,
    tick_arrays: [TickArray; 3],
    amount: u64,
    amount_specified_is_input: bool,
    a_to_b: bool,
) -> [u64; 2] {
    let ta0_refcell = RefCell::new(tick_arrays[0]);
    let ta1_refcell = RefCell::new(tick_arrays[1]);
    let ta2_refcell = RefCell::new(tick_arrays[2]);
    let mut swap_tick_sequence = SwapTickSequence::new(
        ta0_refcell.borrow_mut(),
        Some(ta1_refcell.borrow_mut()),
        Some(ta2_refcell.borrow_mut()),
    );

    // dummy
    let timestamp = whirlpool.reward_last_updated_timestamp;
    let sqrt_price_limit = if a_to_b {
        MIN_SQRT_PRICE_X64
    } else {
        MAX_SQRT_PRICE_X64
    };

    let swap_update = swap(
        whirlpool,
        &mut swap_tick_sequence,
        amount,
        sqrt_price_limit,
        amount_specified_is_input,
        a_to_b,
        timestamp,
    )
    .unwrap();

    [swap_update.amount_a, swap_update.amount_b]
}

pub fn calc_slippage(amount: u64, slippage_num: u64, slippage_denom: u64) -> u64 {
    let num = (slippage_denom - slippage_num) as u128;
    let denom = slippage_denom as u128;
    u64::try_from((amount as u128) * num / denom).unwrap()
}
