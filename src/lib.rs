mod auth;
mod diagnostics;
mod keys;
mod transport;
mod wallet;

use js_sys::{Function, Object, Reflect};
use monero_daemon_rpc::MoneroDaemon;
use monero_wallet::transaction::Transaction;
use monero_wallet::{ViewPair, address::MoneroAddress};
use wasm_bindgen::prelude::*;
use zeroize::Zeroizing;

use auth::Credentials;
use keys::{network_from_u8, point_from_hex, public_key, scalar_from_hex};
use transport::FetchTransport;

fn js_err(message: impl core::fmt::Display) -> JsError {
    JsError::new(&message.to_string())
}

#[wasm_bindgen(js_name = setDiagnosticsEnabled)]
pub fn set_diagnostics_enabled(enabled: bool) {
    diagnostics::set_enabled(enabled);
}

async fn connect(
    daemon_url: &str,
    username: Option<String>,
    password: Option<String>,
) -> Result<MoneroDaemon<FetchTransport>, JsError> {
    connect_with_signal(daemon_url, username, password, None).await
}

async fn connect_with_signal(
    daemon_url: &str,
    username: Option<String>,
    password: Option<String>,
    signal: Option<web_sys::AbortSignal>,
) -> Result<MoneroDaemon<FetchTransport>, JsError> {
    let username = Zeroizing::new(username.unwrap_or_default());
    let password = Zeroizing::new(password.unwrap_or_default());
    let credentials = (!username.is_empty() || !password.is_empty())
        .then_some(Credentials { username, password });
    MoneroDaemon::new(FetchTransport::new(daemon_url, credentials).with_signal(signal))
        .await
        .map_err(|e| js_err(format!("connecting to daemon {daemon_url}: {e}")))
}

fn primary_address_inner(
    network: u8,
    public_spend_key_hex: &str,
    private_view_key_hex: &str,
) -> Result<String, String> {
    let network = network_from_u8(network)?;
    let spend = point_from_hex("public spend key", public_spend_key_hex)?;
    let view = scalar_from_hex("private view key", private_view_key_hex)?;
    let pair = ViewPair::new(spend, view).map_err(|e| e.to_string())?;
    Ok(pair.legacy_address(network).to_string())
}

fn public_key_from_private_inner(private_key_hex: &str) -> Result<String, String> {
    let scalar = scalar_from_hex("private key", private_key_hex)?;
    Ok(hex::encode(public_key(&scalar).compress().to_bytes()))
}

fn transaction_from_hex(tx_hex: &str) -> Result<Transaction, String> {
    let bytes = hex::decode(tx_hex).map_err(|e| format!("invalid transaction hex: {e}"))?;
    let mut remaining = bytes.as_slice();
    let transaction =
        Transaction::read(&mut remaining).map_err(|e| format!("invalid transaction: {e}"))?;
    if !remaining.is_empty() {
        return Err("unexpected trailing transaction bytes".into());
    }
    Ok(transaction)
}

#[wasm_bindgen(js_name = relayTransaction)]
pub async fn relay_transaction(
    daemon_url: String,
    username: Option<String>,
    password: Option<String>,
    tx_hex: String,
) -> Result<String, JsError> {
    let transaction = transaction_from_hex(&tx_hex).map_err(js_err)?;
    let daemon = connect(&daemon_url, username, password).await?;
    wallet::publish(&daemon, &transaction)
        .await
        .map_err(js_err)?;
    Ok(hex::encode(transaction.hash()))
}

/// Primary (legacy) address from the public spend key and private view key.
#[wasm_bindgen(js_name = primaryAddress)]
pub fn primary_address(
    network: u8,
    public_spend_key_hex: &str,
    private_view_key_hex: &str,
) -> Result<String, JsError> {
    primary_address_inner(network, public_spend_key_hex, private_view_key_hex).map_err(js_err)
}

/// Public key (hex) for a private key (hex).
#[wasm_bindgen(js_name = publicKeyFromPrivate)]
pub fn public_key_from_private(private_key_hex: &str) -> Result<String, JsError> {
    public_key_from_private_inner(private_key_hex).map_err(js_err)
}

/// Validates an address string for the given network.
#[wasm_bindgen(js_name = isValidAddress)]
pub fn is_valid_address(network: u8, address: &str) -> bool {
    network_from_u8(network).is_ok_and(|n| MoneroAddress::from_str(n, address).is_ok())
}

/// Chain height (number of blocks) reported by the daemon.
#[wasm_bindgen(js_name = daemonNetwork)]
pub async fn daemon_network(
    daemon_url: String,
    username: Option<String>,
    password: Option<String>,
) -> Result<String, JsError> {
    let daemon = connect(&daemon_url, username, password).await?;
    let response = daemon
        .rpc_call("get_info", None, 64 * 1024)
        .await
        .map_err(js_err)?;
    network_from_daemon_info(&response).map_err(js_err)
}

fn network_from_daemon_info(response: &str) -> Result<String, String> {
    let info: serde_json::Value =
        serde_json::from_str(response).map_err(|error| error.to_string())?;
    match info["nettype"].as_str() {
        Some("fakechain") => Ok("regtest".into()),
        Some(network @ ("mainnet" | "testnet" | "stagenet")) => Ok(network.into()),
        _ => Err("daemon did not report a recognized Monero network".into()),
    }
}

/// Chain height (number of blocks) reported by the daemon.
#[wasm_bindgen(js_name = daemonHeight)]
pub async fn daemon_height(
    daemon_url: String,
    username: Option<String>,
    password: Option<String>,
) -> Result<f64, JsError> {
    let daemon = connect(&daemon_url, username, password).await?;
    let height = wallet::height(&daemon).await.map_err(js_err)?;
    Ok(height as f64)
}

struct FundingControl {
    signal: Option<web_sys::AbortSignal>,
    on_progress: Option<Function>,
}

impl FundingControl {
    fn parse(options: JsValue) -> Result<Self, JsError> {
        if options.is_undefined() || options.is_null() {
            return Ok(Self {
                signal: None,
                on_progress: None,
            });
        }
        if !options.is_object() || js_sys::Array::is_array(&options) || options.is_function() {
            return Err(js_err("scanFunding options must be an object"));
        }
        let signal = Reflect::get(&options, &"signal".into())
            .map_err(|_| js_err("unable to read scanFunding signal"))?;
        let signal = if signal.is_undefined() {
            None
        } else {
            Some(
                signal
                    .dyn_into::<web_sys::AbortSignal>()
                    .map_err(|_| js_err("scanFunding signal must be an AbortSignal"))?,
            )
        };
        let on_progress = Reflect::get(&options, &"onProgress".into())
            .map_err(|_| js_err("unable to read scanFunding onProgress"))?;
        let on_progress = if on_progress.is_undefined() {
            None
        } else {
            Some(
                on_progress
                    .dyn_into::<Function>()
                    .map_err(|_| js_err("scanFunding onProgress must be a function"))?,
            )
        };
        Ok(Self {
            signal,
            on_progress,
        })
    }

    fn check_cancelled(&self) -> Result<(), String> {
        if self.signal.as_ref().is_some_and(|signal| signal.aborted()) {
            return Err("funding scan aborted".into());
        }
        Ok(())
    }

    fn progress(&self, scanned_height: usize, target_height: usize) -> Result<(), String> {
        if let Some(callback) = &self.on_progress {
            let progress = Object::new();
            for (key, height) in [
                ("scannedHeight", scanned_height),
                ("targetHeight", target_height),
            ] {
                Reflect::set(&progress, &key.into(), &(height as f64).into())
                    .map_err(|_| "failed to build funding progress".to_owned())?;
            }
            callback
                .call1(&JsValue::UNDEFINED, &progress)
                .map_err(|error| format!("funding onProgress callback threw: {error:?}"))?;
        }
        Ok(())
    }
}

/// Historical received funding and maturity, without knowledge of spent status.
/// Optional trailing `{ signal?: AbortSignal, onProgress?: Function }` controls this scan only.
/// Progress is `{ scannedHeight, targetHeight }` in block counts after each batch.
/// Aborting or throwing from onProgress rejects the operation; no partial result is returned.
#[wasm_bindgen(js_name = scanFunding)]
#[allow(clippy::too_many_arguments)]
pub async fn scan_funding(
    daemon_url: String,
    username: Option<String>,
    password: Option<String>,
    public_spend_key_hex: String,
    private_view_key_hex: String,
    restore_height: u32,
    options: Option<JsValue>,
) -> Result<JsValue, JsError> {
    let control = FundingControl::parse(options.unwrap_or(JsValue::UNDEFINED))?;
    control.check_cancelled().map_err(js_err)?;
    let spend = point_from_hex("public spend key", &public_spend_key_hex).map_err(js_err)?;
    let view = scalar_from_hex("private view key", &private_view_key_hex).map_err(js_err)?;
    let pair = ViewPair::new(spend, view).map_err(js_err)?;
    let daemon =
        connect_with_signal(&daemon_url, username, password, control.signal.clone()).await?;
    let funding = wallet::scan_funding(
        &daemon,
        &pair,
        restore_height as usize,
        || control.check_cancelled(),
        |scanned, target| control.progress(scanned, target),
    )
    .await
    .map_err(js_err)?;
    let result = Object::new();
    for (key, value) in [
        ("receivedAtomic", funding.received.to_string().into()),
        ("matureAtomic", funding.mature.to_string().into()),
        (
            "lockedAtomic",
            (funding.received - funding.mature).to_string().into(),
        ),
        ("scannedHeight", (funding.scanned_height as f64).into()),
        ("spentStatusKnown", false.into()),
    ] {
        Reflect::set(&result, &JsValue::from_str(key), &value)
            .map_err(|_| js_err("failed to build funding result"))?;
    }
    Ok(result.into())
}

/// Unlocked, unspent balance in atomic units (decimal string), scanning from `restore_height`.
#[wasm_bindgen(js_name = unlockedBalance)]
#[allow(clippy::too_many_arguments)]
pub async fn unlocked_balance(
    daemon_url: String,
    username: Option<String>,
    password: Option<String>,
    private_spend_key_hex: String,
    private_view_key_hex: String,
    restore_height: u32,
    options: Option<JsValue>,
) -> Result<String, JsError> {
    let control = FundingControl::parse(options.unwrap_or(JsValue::UNDEFINED))?;
    control.check_cancelled().map_err(js_err)?;
    diagnostics::event("balance: parsing wallet keys");
    let spend = scalar_from_hex("private spend key", &private_spend_key_hex).map_err(js_err)?;
    let view = scalar_from_hex("private view key", &private_view_key_hex).map_err(js_err)?;
    diagnostics::event("balance: connecting");
    let daemon =
        connect_with_signal(&daemon_url, username, password, control.signal.clone()).await?;
    diagnostics::event("balance: scanning");
    let balance = wallet::unlocked_balance(
        &daemon,
        &spend,
        view,
        restore_height as usize,
        || control.check_cancelled(),
        |scanned, target| control.progress(scanned, target),
    )
    .await
    .map_err(js_err)?;
    diagnostics::event("balance: completed");
    Ok(balance.to_string())
}

#[wasm_bindgen(js_name = scanBalanceBatch)]
#[allow(clippy::too_many_arguments)]
pub async fn scan_balance_batch(
    daemon_url: String,
    username: Option<String>,
    password: Option<String>,
    private_spend_key_hex: String,
    private_view_key_hex: String,
    restore_height: u32,
    checkpoint: Option<String>,
    options: Option<JsValue>,
) -> Result<String, JsError> {
    let control = FundingControl::parse(options.unwrap_or(JsValue::UNDEFINED))?;
    control.check_cancelled().map_err(js_err)?;
    let spend = scalar_from_hex("private spend key", &private_spend_key_hex).map_err(js_err)?;
    let view = scalar_from_hex("private view key", &private_view_key_hex).map_err(js_err)?;
    let daemon =
        connect_with_signal(&daemon_url, username, password, control.signal.clone()).await?;
    wallet::balance_batch(
        &daemon,
        &spend,
        view,
        restore_height as usize,
        checkpoint.as_deref(),
        || control.check_cancelled(),
    )
    .await
    .map_err(js_err)
}

/// Scans, builds and signs a transaction locally; publishes it only if `relay` is true.
///
/// Resolves to `{ hash, feeAtomic, amountAtomic, balanceAtomic, relayed, txHex }`.
#[wasm_bindgen(js_name = createTransaction)]
#[allow(clippy::too_many_arguments)]
pub async fn create_transaction(
    daemon_url: String,
    username: Option<String>,
    password: Option<String>,
    network: u8,
    private_spend_key_hex: String,
    private_view_key_hex: String,
    restore_height: u32,
    recipient: String,
    amount_atomic: String,
    relay: bool,
) -> Result<JsValue, JsError> {
    let amount: u64 = amount_atomic
        .parse()
        .map_err(|_| js_err("amount must be a whole number of atomic units"))?;
    if amount == 0 {
        return Err(js_err("amount must be greater than zero"));
    }
    send(
        daemon_url,
        username,
        password,
        network,
        private_spend_key_hex,
        private_view_key_hex,
        restore_height,
        recipient,
        Some(amount),
        relay,
    )
    .await
}

/// Like `createTransaction`, but sends the whole unlocked balance minus the fee.
///
/// All unlocked outputs are spent in a single transaction, which fails with
/// "too large of a transaction" if the wallet holds very many outputs.
#[wasm_bindgen(js_name = sweepTransaction)]
#[allow(clippy::too_many_arguments)]
pub async fn sweep_transaction(
    daemon_url: String,
    username: Option<String>,
    password: Option<String>,
    network: u8,
    private_spend_key_hex: String,
    private_view_key_hex: String,
    restore_height: u32,
    recipient: String,
    relay: bool,
) -> Result<JsValue, JsError> {
    send(
        daemon_url,
        username,
        password,
        network,
        private_spend_key_hex,
        private_view_key_hex,
        restore_height,
        recipient,
        None,
        relay,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn send(
    daemon_url: String,
    username: Option<String>,
    password: Option<String>,
    network: u8,
    private_spend_key_hex: String,
    private_view_key_hex: String,
    restore_height: u32,
    recipient: String,
    amount: Option<u64>,
    relay: bool,
) -> Result<JsValue, JsError> {
    let network = network_from_u8(network).map_err(js_err)?;
    let spend = scalar_from_hex("private spend key", &private_spend_key_hex).map_err(js_err)?;
    let view = scalar_from_hex("private view key", &private_view_key_hex).map_err(js_err)?;

    let daemon = connect(&daemon_url, username, password).await?;
    let built = wallet::build_transaction(
        &daemon,
        network,
        &spend,
        view,
        restore_height as usize,
        &recipient,
        amount,
    )
    .await
    .map_err(js_err)?;
    if relay {
        wallet::publish(&daemon, &built.transaction)
            .await
            .map_err(js_err)?;
    }

    let result = Object::new();
    let set = |key: &str, value: JsValue| Reflect::set(&result, &key.into(), &value).map(|_| ());
    set("hash", hex::encode(built.transaction.hash()).into())
        .and_then(|()| set("feeAtomic", built.fee.to_string().into()))
        .and_then(|()| set("amountAtomic", built.amount.to_string().into()))
        .and_then(|()| set("balanceAtomic", built.unlocked_balance.to_string().into()))
        .and_then(|()| set("relayed", relay.into()))
        .and_then(|()| set("txHex", hex::encode(built.transaction.serialize()).into()))
        .map_err(|_| js_err("failed to build result object"))?;
    Ok(result.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_network_distinguishes_regtest_from_mainnet() {
        assert_eq!(
            network_from_daemon_info(r#"{"nettype":"fakechain"}"#).unwrap(),
            "regtest"
        );
        for network in ["mainnet", "testnet", "stagenet"] {
            let info = format!(r#"{{"nettype":"{network}"}}"#);
            assert_eq!(network_from_daemon_info(&info).unwrap(), network);
        }
        assert!(network_from_daemon_info("{}").is_err());
        assert!(network_from_daemon_info(r#"{"nettype":"unknown"}"#).is_err());
    }

    #[test]
    fn relay_rejects_malformed_transactions() {
        assert!(transaction_from_hex("not hex").is_err());
        assert!(transaction_from_hex("").is_err());
        assert!(transaction_from_hex("0200").is_err());
    }

    #[test]
    fn derived_address_round_trips() {
        let spend_priv = "77916d0cd56ed1920aef6ca56d8a41bac915b68e4c46a589e0956e27a7b77404";
        let view_priv = "8c1e2ab3c1f8b0d5a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f6070a";
        let spend_pub = public_key_from_private_inner(spend_priv).unwrap();
        let address = primary_address_inner(0, &spend_pub, view_priv).unwrap();
        assert!(address.starts_with('4'));
        assert!(is_valid_address(0, &address));
        assert!(!is_valid_address(2, &address));
    }
}
