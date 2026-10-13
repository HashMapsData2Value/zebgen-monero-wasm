mod auth;
mod keys;
mod transport;
mod wallet;

use js_sys::{Object, Reflect};
use monero_daemon_rpc::MoneroDaemon;
use monero_wallet::{ViewPair, address::MoneroAddress};
use wasm_bindgen::prelude::*;
use zeroize::Zeroizing;

use auth::Credentials;
use keys::{network_from_u8, point_from_hex, public_key, scalar_from_hex};
use transport::FetchTransport;

fn js_err(message: impl core::fmt::Display) -> JsError {
  JsError::new(&message.to_string())
}

async fn connect(
  daemon_url: &str,
  username: Option<String>,
  password: Option<String>,
) -> Result<MoneroDaemon<FetchTransport>, JsError> {
  let username = Zeroizing::new(username.unwrap_or_default());
  let password = Zeroizing::new(password.unwrap_or_default());
  let credentials =
    (!username.is_empty() || !password.is_empty()).then_some(Credentials { username, password });
  MoneroDaemon::new(FetchTransport::new(daemon_url, credentials))
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

/// Unlocked, unspent balance in atomic units (decimal string), scanning from `restore_height`.
#[wasm_bindgen(js_name = unlockedBalance)]
pub async fn unlocked_balance(
  daemon_url: String,
  username: Option<String>,
  password: Option<String>,
  private_spend_key_hex: String,
  private_view_key_hex: String,
  restore_height: u32,
) -> Result<String, JsError> {
  let spend = scalar_from_hex("private spend key", &private_spend_key_hex).map_err(js_err)?;
  let view = scalar_from_hex("private view key", &private_view_key_hex).map_err(js_err)?;
  let daemon = connect(&daemon_url, username, password).await?;
  let balance = wallet::unlocked_balance(&daemon, &spend, view, restore_height as usize)
    .await
    .map_err(js_err)?;
  Ok(balance.to_string())
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
  let network = network_from_u8(network).map_err(js_err)?;
  let spend = scalar_from_hex("private spend key", &private_spend_key_hex).map_err(js_err)?;
  let view = scalar_from_hex("private view key", &private_view_key_hex).map_err(js_err)?;
  let amount: u64 =
    amount_atomic.parse().map_err(|_| js_err("amount must be a whole number of atomic units"))?;
  if amount == 0 {
    return Err(js_err("amount must be greater than zero"));
  }

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
    wallet::publish(&daemon, &built.transaction).await.map_err(js_err)?;
  }

  let result = Object::new();
  let set = |key: &str, value: JsValue| Reflect::set(&result, &key.into(), &value).map(|_| ());
  set("hash", hex::encode(built.transaction.hash()).into())
    .and_then(|()| set("feeAtomic", built.fee.to_string().into()))
    .and_then(|()| set("amountAtomic", amount.to_string().into()))
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
