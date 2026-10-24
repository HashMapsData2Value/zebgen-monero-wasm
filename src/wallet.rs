use core::{cmp::Reverse, ops::Deref as _};

use monero_daemon_rpc::{HttpTransport, MoneroDaemon};
use monero_wallet::{
    DEFAULT_LOCK_WINDOW, OutputWithDecoys, Scanner, ViewPair, WalletOutput,
    address::{MoneroAddress, Network},
    ed25519::{Point, Scalar},
    interface::prelude::*,
    ringct::RctType,
    send::{Change, SendError, SignableTransaction},
    transaction::{Timelock, Transaction},
};
use rand_core::{OsRng, RngCore as _};
use zeroize::Zeroizing;

use crate::keys::public_key;

const SCAN_BATCH: usize = 100;
const RING_LEN: u8 = 16;
// Cap on the daemon-suggested fee rate (atomic units per weight) against a malicious node.
// Leaves room for regtest/young chains (high block reward -> ~4.7M "normal" on our regtest).
const MAX_FEE_PER_WEIGHT: u64 = 10_000_000;

pub struct BuiltTransaction {
    pub transaction: Transaction,
    pub amount: u64,
    pub fee: u64,
    pub unlocked_balance: u64,
}

pub struct Funding {
    pub received: u64,
    pub mature: u64,
    pub scanned_height: usize,
}

pub async fn scan_funding<T: HttpTransport>(
    daemon: &MoneroDaemon<T>,
    pair: &ViewPair,
    restore_height: usize,
    check_cancelled: impl Fn() -> Result<(), String>,
    on_progress: impl Fn(usize, usize) -> Result<(), String>,
) -> Result<Funding, String> {
    check_cancelled()?;
    let tip = daemon
        .latest_block_number()
        .await
        .map_err(|error| error.to_string())?;
    check_cancelled()?;
    if restore_height > tip + 1 {
        return Err("restore height is beyond the daemon height".into());
    }
    let mut scanner = Scanner::new(pair.clone());
    let mut funding = Funding {
        received: 0,
        mature: 0,
        scanned_height: tip + 1,
    };
    let mut start = restore_height;
    while start <= tip {
        check_cancelled()?;
        let end = (start + SCAN_BATCH - 1).min(tip);
        let blocks = daemon
            .contiguous_scannable_blocks(start..=end)
            .await
            .map_err(|error| error.to_string())?;
        check_cancelled()?;
        for block in blocks {
            let number = block.block.number();
            let outputs = scanner.scan(block).map_err(|error| error.to_string())?;
            for output in outputs.ignore_additional_timelock() {
                let amount = output.commitment().amount;
                funding.received = funding
                    .received
                    .checked_add(amount)
                    .ok_or("received amount overflow")?;
                if number + DEFAULT_LOCK_WINDOW <= tip + 1
                    && output.additional_timelock() <= Timelock::Block(tip)
                {
                    funding.mature = funding
                        .mature
                        .checked_add(amount)
                        .ok_or("mature amount overflow")?;
                }
            }
        }
        start = end + 1;
        on_progress(start, tip + 1)?;
        check_cancelled()?;
    }
    Ok(funding)
}

fn key_image(spend_key: &Scalar, output: &WalletOutput) -> [u8; 32] {
    let input_key = Zeroizing::new(spend_key.clone().into() + output.key_offset().into());
    let hashed = Point::biased_hash(output.key().compress().to_bytes()).into();
    Point::from(input_key.deref() * hashed)
        .compress()
        .to_bytes()
}

async fn scan_unlocked<T: HttpTransport>(
    daemon: &MoneroDaemon<T>,
    pair: &ViewPair,
    restore_height: usize,
    tip: usize,
    check_cancelled: impl Fn() -> Result<(), String>,
    on_progress: impl Fn(usize, usize) -> Result<(), String>,
) -> Result<Vec<WalletOutput>, String> {
    let mut scanner = Scanner::new(pair.clone());
    let mut found = vec![];
    let mut start = restore_height;
    while start <= tip {
        check_cancelled()?;
        let end = (start + SCAN_BATCH - 1).min(tip);
        crate::diagnostics::event(&format!("scan: requesting blocks {start}..={end}"));
        let blocks = daemon
            .contiguous_scannable_blocks(start..=end)
            .await
            .map_err(|e| e.to_string())?;
        check_cancelled()?;
        for block in blocks {
            let number = block.block.number();
            let outputs = scanner.scan(block).map_err(|e| e.to_string())?;
            if number + DEFAULT_LOCK_WINDOW > tip + 1 {
                continue;
            }
            // Coinbase outputs carry a Block timelock; time-based locks are treated as still locked.
            found.extend(outputs.additional_timelock_satisfied_by(tip, 0));
        }
        start = end + 1;
        on_progress(start, tip + 1)?;
        check_cancelled()?;
    }
    crate::diagnostics::event(&format!("scan: {} unlocked outputs", found.len()));
    Ok(found)
}

async fn filter_unspent<T: HttpTransport>(
    daemon: &MoneroDaemon<T>,
    spend_key: &Scalar,
    outputs: Vec<WalletOutput>,
) -> Result<Vec<WalletOutput>, String> {
    if outputs.is_empty() {
        return Ok(outputs);
    }
    crate::diagnostics::event(&format!("spent check: {} outputs", outputs.len()));
    let key_images: Vec<String> = outputs
        .iter()
        .map(|o| hex::encode(key_image(spend_key, o)))
        .collect();
    crate::diagnostics::event("spent check: key images computed");
    let params = serde_json::json!({ "key_images": key_images }).to_string();
    // `rpc_call` is outside monero-daemon-rpc's SemVer guarantees; safe because the crate is pinned exactly.
    let response = daemon
        .rpc_call(
            "is_key_image_spent",
            Some(params),
            1024 + 16 * outputs.len(),
        )
        .await
        .map_err(|e| e.to_string())?;
    crate::diagnostics::event("spent check: RPC response received");
    let parsed: serde_json::Value = serde_json::from_str(&response).map_err(|e| e.to_string())?;
    let statuses = parsed["spent_status"]
        .as_array()
        .filter(|s| s.len() == outputs.len())
        .ok_or("daemon returned a malformed is_key_image_spent response")?;
    crate::diagnostics::event("spent check: response validated");
    // 0 = unspent; 1 = spent on chain; 2 = spent in the mempool.
    Ok(outputs
        .into_iter()
        .zip(statuses)
        .filter(|(_, status)| status.as_u64() == Some(0))
        .map(|(output, _)| output)
        .collect())
}

/// Chain height (number of blocks), matching monero-ts's `getHeight()`.
pub async fn height<T: HttpTransport>(daemon: &MoneroDaemon<T>) -> Result<usize, String> {
    Ok(daemon
        .latest_block_number()
        .await
        .map_err(|e| e.to_string())?
        + 1)
}

pub async fn unlocked_balance<T: HttpTransport>(
    daemon: &MoneroDaemon<T>,
    spend_key: &Zeroizing<Scalar>,
    view_key: Zeroizing<Scalar>,
    restore_height: usize,
    check_cancelled: impl Fn() -> Result<(), String>,
    on_progress: impl Fn(usize, usize) -> Result<(), String>,
) -> Result<u64, String> {
    check_cancelled()?;
    let pair = ViewPair::new(public_key(spend_key), view_key).map_err(|e| e.to_string())?;
    let tip = daemon
        .latest_block_number()
        .await
        .map_err(|e| e.to_string())?;
    check_cancelled()?;
    if restore_height > tip + 1 {
        return Err("restore height is beyond the daemon height".into());
    }
    let outputs = scan_unlocked(
        daemon,
        &pair,
        restore_height,
        tip,
        &check_cancelled,
        on_progress,
    )
    .await?;
    check_cancelled()?;
    let unspent = filter_unspent(daemon, spend_key, outputs).await?;
    check_cancelled()?;
    Ok(unspent.iter().map(|o| o.commitment().amount).sum())
}

fn next_scan_gap(
    spans: &[(usize, usize, String)],
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    let mut cursor = start;
    for (from, to, _) in spans {
        if *to <= cursor {
            continue;
        }
        if *from > cursor {
            return (cursor < end).then_some((cursor, (*from).min(end)));
        }
        cursor = cursor.max(*to);
    }
    (cursor < end).then_some((cursor, end))
}

#[cfg(test)]
mod coverage_tests {
    use super::next_scan_gap;

    #[test]
    fn scans_only_missing_intervals() {
        let spans = vec![(1000, 2000, String::new()), (2500, 3000, String::new())];
        assert_eq!(next_scan_gap(&spans, 1000, 3000), Some((2000, 2500)));
        assert_eq!(next_scan_gap(&spans, 2500, 3000), None);
        assert_eq!(next_scan_gap(&spans, 0, 3000), Some((0, 1000)));
        assert_eq!(next_scan_gap(&spans, 3000, 3100), Some((3000, 3100)));
        assert_eq!(next_scan_gap(&spans, 0, 500), Some((0, 500)));
    }
}

pub async fn balance_batch<T: HttpTransport>(
    daemon: &MoneroDaemon<T>,
    spend_key: &Zeroizing<Scalar>,
    view_key: Zeroizing<Scalar>,
    restore_height: usize,
    saved: Option<&str>,
    check_cancelled: impl Fn() -> Result<(), String>,
) -> Result<String, String> {
    let tip = daemon
        .latest_block_number()
        .await
        .map_err(|error| error.to_string())?;
    if restore_height > tip + 1 {
        return Err("restore height is beyond the daemon height".into());
    }
    let mut state = match saved {
        Some(text) => {
            serde_json::from_str::<serde_json::Value>(text).map_err(|error| error.to_string())?
        }
        None => serde_json::json!({"version": 2, "spans": [], "outputs": []}),
    };
    let identity = hex::encode(public_key(spend_key).compress().to_bytes());
    let view_identity = hex::encode(public_key(&view_key).compress().to_bytes());
    if saved.is_some()
        && (state["publicSpendKey"].as_str() != Some(&identity)
            || state["publicViewKey"].as_str() != Some(&view_identity))
    {
        return Err("scan checkpoint belongs to another wallet".into());
    }
    if state["version"].as_u64() == Some(1) {
        let start = state["restoreHeight"]
            .as_u64()
            .ok_or("invalid legacy restore height")?;
        let end = state["nextHeight"]
            .as_u64()
            .ok_or("invalid legacy scan height")?;
        if start > end {
            return Err("invalid legacy scan range".into());
        }
        state["spans"] = if start < end {
            serde_json::json!([{ "start": start, "end": end, "anchorHash": state["anchorHash"] }])
        } else {
            serde_json::json!([])
        };
        state["version"] = 2.into();
    }
    if state["version"].as_u64() != Some(2) {
        return Err("invalid scan checkpoint version".into());
    }
    let mut spans = Vec::new();
    for span in state["spans"].as_array().ok_or("invalid scan spans")? {
        let start = span["start"].as_u64().ok_or("invalid scan span start")? as usize;
        let end = span["end"].as_u64().ok_or("invalid scan span end")? as usize;
        let anchor = span["anchorHash"]
            .as_str()
            .filter(|hash| hash.len() == 64)
            .ok_or("invalid scan span anchor")?
            .to_owned();
        if start >= end
            || spans
                .last()
                .is_some_and(|previous: &(usize, usize, String)| previous.1 >= start)
        {
            return Err("invalid or overlapping scan spans".into());
        }
        spans.push((start, end, anchor));
    }
    for (_, end, anchor) in &spans {
        check_cancelled()?;
        let matching = if *end - 1 > tip {
            false
        } else {
            let response = daemon
                .json_rpc_call(
                    "get_block_header_by_height",
                    Some(serde_json::json!({"height": *end - 1}).to_string()),
                    8192,
                )
                .await
                .map_err(|error| error.to_string())?;
            let header: serde_json::Value =
                serde_json::from_str(&response).map_err(|error| error.to_string())?;
            header["block_header"]["hash"]
                .as_str()
                .is_some_and(|hash| hash == anchor)
        };
        if !matching {
            state = serde_json::json!({"version": 2, "spans": [], "outputs": []});
            spans.clear();
            break;
        }
    }
    check_cancelled()?;
    let pair = ViewPair::new(public_key(spend_key), view_key).map_err(|error| error.to_string())?;
    let mut scanner = Scanner::new(pair);
    let gap = next_scan_gap(&spans, restore_height, tip + 1);
    let entries = state["outputs"]
        .as_array_mut()
        .ok_or("invalid output checkpoint")?;
    if let Some((next, gap_end)) = gap {
        let end = (next + SCAN_BATCH).min(gap_end);
        let expected_previous = spans
            .iter()
            .find(|span| span.1 == next)
            .map(|span| span.2.clone());
        let mut scanned_anchor = None;
        let blocks = daemon
            .contiguous_scannable_blocks(next..=end - 1)
            .await
            .map_err(|error| error.to_string())?;
        check_cancelled()?;
        for block in blocks {
            let height = block.block.number();
            if height == next
                && expected_previous
                    .as_ref()
                    .is_some_and(|expected| hex::encode(block.block.header.previous) != *expected)
            {
                return Err(
                    "chain reorganized at scan boundary; retry from the last checkpoint".into(),
                );
            }
            if height == end - 1 {
                scanned_anchor = Some(hex::encode(block.block.hash()));
            }
            for output in scanner
                .scan(block)
                .map_err(|error| error.to_string())?
                .ignore_additional_timelock()
            {
                entries.push(serde_json::json!({"height": height, "output": hex::encode(output.serialize())}));
            }
        }
        let anchor = scanned_anchor.ok_or("daemon omitted scan batch blocks")?;
        spans.push((next, end, anchor));
        spans.sort_by_key(|span| span.0);
        let mut merged: Vec<(usize, usize, String)> = Vec::new();
        for span in spans {
            if let Some(previous) = merged.last_mut().filter(|previous| previous.1 == span.0) {
                previous.1 = span.1;
                previous.2 = span.2;
            } else {
                merged.push(span);
            }
        }
        spans = merged;
    }
    let next = next_scan_gap(&spans, restore_height, tip + 1).map_or(tip + 1, |gap| gap.0);
    let mut mature = vec![];
    let mut locked = vec![];
    for entry in entries.iter() {
        let height = entry["height"]
            .as_u64()
            .ok_or("invalid saved output height")? as usize;
        if !spans.iter().any(|span| height >= span.0 && height < span.1) {
            return Err("saved output outside scanned range".into());
        }
        let bytes = Zeroizing::new(
            hex::decode(entry["output"].as_str().ok_or("invalid saved output")?)
                .map_err(|error| error.to_string())?,
        );
        let mut remaining = bytes.as_slice();
        let output = WalletOutput::read(&mut remaining).map_err(|error| error.to_string())?;
        if !remaining.is_empty() {
            return Err("invalid saved output bytes".into());
        }
        if height + DEFAULT_LOCK_WINDOW <= tip + 1
            && output.additional_timelock() <= Timelock::Block(tip)
        {
            mature.push(output);
        } else {
            locked.push(output);
        }
    }
    let locked_balance = if next == tip + 1 {
        filter_unspent(daemon, spend_key, locked)
            .await?
            .iter()
            .try_fold(0u64, |total, output| {
                total.checked_add(output.commitment().amount)
            })
            .ok_or("locked balance overflow")?
    } else {
        0
    };
    let balance = if next == tip + 1 {
        filter_unspent(daemon, spend_key, mature)
            .await?
            .iter()
            .try_fold(0u64, |total, output| {
                total.checked_add(output.commitment().amount)
            })
            .ok_or("balance overflow")?
    } else {
        0
    };
    for (_, end, expected_anchor) in &spans {
        check_cancelled()?;
        let response = daemon
            .json_rpc_call(
                "get_block_header_by_height",
                Some(serde_json::json!({"height": *end - 1}).to_string()),
                8192,
            )
            .await
            .map_err(|error| error.to_string())?;
        let header: serde_json::Value =
            serde_json::from_str(&response).map_err(|error| error.to_string())?;
        let anchor = header["block_header"]["hash"]
            .as_str()
            .filter(|hash| hash.len() == 64)
            .ok_or("invalid scan anchor hash")?;
        if expected_anchor != anchor {
            return Err("chain reorganized during scan; retry from the last checkpoint".into());
        }
    }
    state["publicSpendKey"] = identity.into();
    state["publicViewKey"] = view_identity.into();
    state["restoreHeight"] = restore_height.into();
    state["nextHeight"] = next.into();
    state["spans"] = spans.iter().map(|(start, end, anchor)| serde_json::json!({"start": start, "end": end, "anchorHash": anchor})).collect();
    check_cancelled()?;
    Ok(serde_json::json!({"checkpoint": state, "scannedHeight": next, "targetHeight": tip + 1, "done": next == tip + 1, "balanceAtomic": balance.to_string(), "lockedAtomic": locked_balance.to_string()}).to_string())
}

/// Builds and signs a payment of `amount`, or of the whole unlocked balance minus the fee when
/// `amount` is `None` (a sweep).
#[allow(clippy::too_many_arguments)]
pub async fn build_transaction<T: HttpTransport>(
    daemon: &MoneroDaemon<T>,
    network: Network,
    spend_key: &Zeroizing<Scalar>,
    view_key: Zeroizing<Scalar>,
    restore_height: usize,
    recipient: &str,
    amount: Option<u64>,
) -> Result<BuiltTransaction, String> {
    let recipient = MoneroAddress::from_str(network, recipient)
        .map_err(|e| format!("invalid recipient address: {e}"))?;
    let pair = ViewPair::new(public_key(spend_key), view_key).map_err(|e| e.to_string())?;
    let tip = daemon
        .latest_block_number()
        .await
        .map_err(|e| e.to_string())?;

    let outputs =
        scan_unlocked(daemon, &pair, restore_height, tip, || Ok(()), |_, _| Ok(())).await?;
    let mut outputs = filter_unspent(daemon, spend_key, outputs).await?;
    let unlocked_balance: u64 = outputs.iter().map(|o| o.commitment().amount).sum();
    outputs.sort_by_key(|o| Reverse(o.commitment().amount));

    let fee_rate = daemon
        .fee_rate(FeePriority::Normal, MAX_FEE_PER_WEIGHT)
        .await
        .map_err(|e| format!("fee rate: {e}"))?;
    let mut outgoing_view_key = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(outgoing_view_key.as_mut());
    let signable_for = |inputs: &[OutputWithDecoys], amount: u64| {
        SignableTransaction::new(
            RctType::ClsagBulletproofPlus,
            outgoing_view_key.clone(),
            inputs.to_vec(),
            vec![(recipient, amount)],
            // A sweep still gets a (zero-amount) change output: Monero needs two outputs, as wallet2 does.
            Change::new(pair.clone(), None),
            vec![],
            fee_rate,
        )
    };

    let Some(amount) = amount else {
        if outputs.is_empty() {
            return Err("no unlocked funds to send".to_owned());
        }
        let mut inputs = Vec::with_capacity(outputs.len());
        for output in outputs {
            inputs.push(
                OutputWithDecoys::new(&mut OsRng, daemon, RING_LEN, tip, output)
                    .await
                    .map_err(|e| format!("decoy selection: {e}"))?,
            );
        }
        // The fee depends only on the transaction's shape (inputs, outputs), not on the amounts.
        let fee = signable_for(&inputs, 1)
            .map_err(|e| e.to_string())?
            .necessary_fee();
        let amount = unlocked_balance
            .checked_sub(fee)
            .filter(|a| *a > 0)
            .ok_or_else(|| {
                format!("unlocked balance {unlocked_balance} does not cover the fee {fee}")
            })?;
        let signable = signable_for(&inputs, amount).map_err(|e| e.to_string())?;
        if signable.necessary_fee() != fee {
            return Err("fee changed between estimate and build".to_owned());
        }
        let transaction = signable
            .sign(&mut OsRng, spend_key)
            .map_err(|e| e.to_string())?;
        return Ok(BuiltTransaction {
            transaction,
            amount,
            fee,
            unlocked_balance,
        });
    };

    let insufficient =
        || format!("insufficient unlocked funds: have {unlocked_balance}, need {amount} plus fee");
    let mut remaining = outputs.into_iter();
    let mut inputs: Vec<OutputWithDecoys> = vec![];
    let mut input_sum = 0u64;
    let mut need_more = true;
    let signable = loop {
        // Add inputs until the payment is covered; a fee shortfall adds one more per retry.
        while need_more || input_sum < amount {
            let output = remaining.next().ok_or_else(insufficient)?;
            input_sum = input_sum.saturating_add(output.commitment().amount);
            inputs.push(
                OutputWithDecoys::new(&mut OsRng, daemon, RING_LEN, tip, output)
                    .await
                    .map_err(|e| format!("decoy selection: {e}"))?,
            );
            need_more = false;
        }
        match signable_for(&inputs, amount) {
            Ok(signable) => break signable,
            Err(SendError::NotEnoughFunds { .. }) => need_more = true,
            Err(e) => return Err(e.to_string()),
        }
    };

    let fee = signable.necessary_fee();
    let transaction = signable
        .sign(&mut OsRng, spend_key)
        .map_err(|e| e.to_string())?;
    Ok(BuiltTransaction {
        transaction,
        amount,
        fee,
        unlocked_balance,
    })
}

pub async fn publish<T: HttpTransport>(
    daemon: &MoneroDaemon<T>,
    transaction: &Transaction,
) -> Result<(), String> {
    daemon
        .publish_transaction(transaction)
        .await
        .map_err(|e| e.to_string())
}
