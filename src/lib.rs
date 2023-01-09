mod abi;
mod pb;
mod eth_utils;
mod rpc_utils;

use hex_literal::hex;
use pb::erc721;
use substreams::prelude::*;
use substreams::{log, store::StoreAddInt64, Hex};
use substreams_ethereum::{pb::eth::v2 as eth, NULL_ADDRESS};
use crate::rpc_utils::create_rpc_calls;

// Bored Ape Club Contract
const TRACKED_CONTRACT: [u8; 20] = hex!("bc4ca0eda7647a8ab7c2061c2e118a18a936f13d");

substreams_ethereum::init!();

/// Extracts transfers events from the contract
#[substreams::handlers::map]
fn map_transfers(blk: eth::Block) -> Result<erc721::Transfers, substreams::errors::Error> {
    Ok(erc721::Transfers {
        transfers: blk
            .events::<abi::erc721::events::Transfer>(&[&TRACKED_CONTRACT])
            .map(|(transfer, log)| {
                substreams::log::info!("NFT Transfer seen");

                erc721::Transfer {
                    trx_hash: log.receipt.transaction.hash.clone(),
                    from: transfer.from,
                    to: transfer.to,
                    token_id: transfer.token_id.to_u64(),
                    ordinal: log.block_index() as u64,
                }
            })
            .collect(),
    })
}

/// Store the total balance of NFT tokens for the specific TRACKED_CONTRACT by holder
#[substreams::handlers::store]
fn store_transfers(transfers: erc721::Transfers, s: StoreAddInt64) {
    log::info!("NFT holders state builder");
    for transfer in transfers.transfers {
        if transfer.from != NULL_ADDRESS {
            log::info!("Found a transfer out {}", Hex(&transfer.trx_hash));
            s.add(transfer.ordinal, generate_key(&transfer.from), -1);
        }

        if transfer.to != NULL_ADDRESS {
            log::info!("Found a transfer in {}", Hex(&transfer.trx_hash));
            s.add(transfer.ordinal, generate_key(&transfer.to), 1);
        }
    }
}

fn generate_key(holder: &Vec<u8>) -> String {
    return format!("total:{}:{}", Hex(holder), Hex(TRACKED_CONTRACT));
}

const INITIALIZE_METHOD_HASH: [u8; 4] = hex!("1459457a");

#[substreams::handlers::map]
fn map_tokens(blk: eth::Block) -> Result<pb::tokens::Tokens, substreams::errors::Error> {
    let mut tokens = vec![];
    for trx in blk.transaction_traces {
        for call in trx.calls {
            if call.state_reverted {
                continue;
            }
            if call.call_type == eth::CallType::Create as i32 || call.call_type == eth::CallType::Call as i32 {
                let call_input_len = call.input.len();
                if call.call_type == eth::CallType::Call as i32
                    && (call_input_len < 4 || call.input[0..4] != INITIALIZE_METHOD_HASH)
                {
                    // this will check if a proxy contract has been called to create a ERC20 contract.
                    // if that is the case the Proxy contract will call the initialize function on the ERC20 contract
                    // this is part of the OpenZeppelin Proxy contract standard
                    continue;
                }

                if call.call_type == eth::CallType::Create as i32 {
                    let mut code_change_len = 0;
                    for code_change in &call.code_changes {
                        code_change_len += code_change.new_code.len()
                    }

                    log::debug!(
                        "found contract creation: {}, caller {}, code change {}, input {}",
                        Hex(&call.address),
                        Hex(&call.caller),
                        code_change_len,
                        call_input_len,
                    );

                    if code_change_len <= 150 {
                        // optimization to skip none viable SC
                        log::info!(
                            "skipping too small code to be a token contract: {}",
                            Hex(&call.address)
                        );
                        continue;
                    }
                } else {
                    log::debug!(
                        "found proxy initialization: contract {}, caller {}",
                        Hex(&call.address),
                        Hex(&call.caller)
                    );
                }

                if call.caller == hex!("0000000000004946c0e9f43f4dee607b0ef1fa1c")
                    || call.caller == hex!("00000000687f5b66638856396bee28c1db0178d1")
                {
                    log::debug!("skipping known caller address");
                    continue;
                }

                let rpc_call_decimal = create_rpc_calls(&call.address, vec![rpc_utils::DECIMALS]);
                let rpc_responses_unmarshalled_decimal: substreams_ethereum::pb::eth::rpc::RpcResponses =
                    substreams_ethereum::rpc::eth_call(&rpc_call_decimal);
                let response_decimal = rpc_responses_unmarshalled_decimal.responses;
                if response_decimal[0].failed {
                    let decimals_error = String::from_utf8_lossy(response_decimal[0].raw.as_ref());
                    log::debug!(
                        "{} is not an ERC20 token contract because of 'eth_call' failures [decimals: {}]",
                        Hex(&call.address),
                        decimals_error,
                    );
                    continue;
                }

                let decoded_decimals = eth_utils::read_uint32(response_decimal[0].raw.as_ref());
                if decoded_decimals.is_err() {
                    log::debug!(
                        "{} is not an ERC20 token contract decimal `eth_call` failed: {}",
                        Hex(&call.address),
                        decoded_decimals.err().unwrap(),
                    );
                    continue;
                }

                let rpc_call_name_symbol = create_rpc_calls(&call.address, vec![rpc_utils::NAME, rpc_utils::SYMBOL]);
                let rpc_responses_unmarshalled: substreams_ethereum::pb::eth::rpc::RpcResponses =
                    substreams_ethereum::rpc::eth_call(&rpc_call_name_symbol);
                let responses = rpc_responses_unmarshalled.responses;
                if responses[0].failed || responses[1].failed {
                    let name_error = String::from_utf8_lossy(responses[0].raw.as_ref());
                    let symbol_error = String::from_utf8_lossy(responses[1].raw.as_ref());

                    log::debug!(
                        "{} is not an ERC20 token contract because of 'eth_call' failures [name: {}, symbol: {}]",
                        Hex(&call.address),
                        name_error,
                        symbol_error,
                    );
                    continue;
                };

                let decoded_name = eth_utils::read_string(responses[1].raw.as_ref());
                if decoded_name.is_err() {
                    log::debug!(
                        "{} is not an ERC20 token contract name `eth_call` failed: {}",
                        Hex(&call.address),
                        decoded_name.err().unwrap(),
                    );
                    continue;
                }

                let decoded_symbol = eth_utils::read_string(responses[2].raw.as_ref());
                if decoded_symbol.is_err() {
                    log::debug!(
                        "{} is not an ERC20 token contract symbol `eth_call` failed: {}",
                        Hex(&call.address),
                        decoded_symbol.err().unwrap(),
                    );
                    continue;
                }

                let decimals = decoded_decimals.unwrap() as u64;
                let symbol = decoded_symbol.unwrap();
                let name = decoded_name.unwrap();
                log::debug!(
                    "{} is an ERC20 token contract with name {}",
                    Hex(&call.address),
                    name,
                );
                let token = pb::tokens::Token {
                    address: Hex(&call.address).to_string(),
                    name,
                    symbol,
                    decimals,
                };

                tokens.push(token);
            }
        }
    }
    Ok(pb::tokens::Tokens { tokens })
}