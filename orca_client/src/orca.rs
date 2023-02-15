use anchor_client::Program;
use anchor_lang::{
    prelude::*,
    solana_program::{instruction::Instruction, pubkey},
    AccountDeserialize,
};
use anyhow::Result;
use solana_client_helpers::{
    spl_associated_token_account::get_associated_token_address, Client as SolanaClient,
};
use spl_token::ID as TOKEN_PROGRAM_ID;
use whirlpool::{
    math::MIN_SQRT_PRICE_X64,
    state::{TickArray, Whirlpool},
};

use crate::orca_utils::*;

const ORCA_WHIRLPOOL_PROGRAM_ID: Pubkey = pubkey!("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc");

// SAMO/USDC(64) whirlpool need to be cloned on local-validator
const SAMO_USDC_WHIRLPOOL_ADDRESS: Pubkey = pubkey!("9vqYJjDUFecLL2xPUC4Rc7hyCtZ6iJ4mDiVZX7aFXoAe");

pub fn swap(
    amount_in: u64,
    solana_client: &SolanaClient,
    program: Program,
    payer_pubkey: Pubkey,
) -> Result<Vec<Instruction>> {
    // swap input
    let a_to_b = true;

    // get whirlpool
    let mut whirlpool_data: &[u8] = &solana_client
        .get_account_data(&SAMO_USDC_WHIRLPOOL_ADDRESS)
        .unwrap();
    let whirlpool = Whirlpool::try_deserialize(&mut whirlpool_data).unwrap();

    println!(
        "whirlpool token_mint_a {}",
        whirlpool.token_mint_a.to_string()
    );
    println!(
        "whirlpool token_mint_b {}",
        whirlpool.token_mint_b.to_string()
    );
    println!(
        "whirlpool token_vault_a {}",
        whirlpool.token_vault_a.to_string()
    );
    println!(
        "whirlpool token_vault_b {}",
        whirlpool.token_vault_b.to_string()
    );
    println!("whirlpool tick_spacing {}", whirlpool.tick_spacing);
    println!(
        "whirlpool tick_current_index {}",
        whirlpool.tick_current_index
    );
    println!("whirlpool sqrt_price {}", whirlpool.sqrt_price);

    // get tickarray for swap
    let tick_arrays = poolutil_get_tick_array_pubkeys_for_swap(
        whirlpool.tick_current_index,
        whirlpool.tick_spacing,
        a_to_b,
        &ORCA_WHIRLPOOL_PROGRAM_ID,
        &SAMO_USDC_WHIRLPOOL_ADDRESS,
    );
    let mut ta0_data: &[u8] = &solana_client.get_account_data(&tick_arrays[0]).unwrap();
    let mut ta1_data: &[u8] = &solana_client.get_account_data(&tick_arrays[1]).unwrap();
    let mut ta2_data: &[u8] = &solana_client.get_account_data(&tick_arrays[2]).unwrap();
    let ta0 = TickArray::try_deserialize(&mut ta0_data).unwrap();
    let ta1 = TickArray::try_deserialize(&mut ta1_data).unwrap();
    let ta2 = TickArray::try_deserialize(&mut ta2_data).unwrap();

    println!("tick_arrays[0] {}", tick_arrays[0].to_string());
    println!("tick_arrays[1] {}", tick_arrays[1].to_string());
    println!("tick_arrays[2] {}", tick_arrays[2].to_string());

    // get quote
    let [quote_amount_in, quote_amount_out] = get_swap_quote(
        &whirlpool,
        [ta0, ta1, ta2],
        amount_in,
        true, // amount is input amount
        a_to_b,
    );
    let amount_out = calc_slippage(quote_amount_out, 1, 100); // 1%
    println!("quote amount_in {}", quote_amount_in);
    println!("quote amount_out {}", quote_amount_out);
    println!("amount_out (slippage included) {}", amount_out);

    // get oracle
    let oracle = pdautil_get_oracle(&ORCA_WHIRLPOOL_PROGRAM_ID, &SAMO_USDC_WHIRLPOOL_ADDRESS);

    // get ATA
    // - Assume that the ATA has already been created
    // - If one token of pair is SOL, the WSOL account must be processed (avoid SOL in this example)
    let ata_a = get_associated_token_address(&payer_pubkey, &whirlpool.token_mint_a);
    let ata_b = get_associated_token_address(&payer_pubkey, &whirlpool.token_mint_b);
    println!("ata_a {}", ata_a.to_string());
    println!("ata_b {}", ata_b.to_string());

    // execute proxy_swap
    let instructions = program
        .request()
        .accounts(whirlpool::accounts::Swap {
            whirlpool: SAMO_USDC_WHIRLPOOL_ADDRESS,
            token_program: TOKEN_PROGRAM_ID,
            token_authority: payer_pubkey,
            token_owner_account_a: ata_a,
            token_owner_account_b: ata_b,
            token_vault_a: whirlpool.token_vault_a,
            token_vault_b: whirlpool.token_vault_b,
            tick_array_0: tick_arrays[0],
            tick_array_1: tick_arrays[1],
            tick_array_2: tick_arrays[2],
            oracle,
        })
        .args(whirlpool::instruction::Swap {
            a_to_b: a_to_b,
            amount_specified_is_input: true,
            other_amount_threshold: amount_out,
            sqrt_price_limit: MIN_SQRT_PRICE_X64, // a to b
            amount: amount_in,
        })
        .instructions()?;

    Ok(instructions)
}
