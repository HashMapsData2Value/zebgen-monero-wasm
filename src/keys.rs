use monero_wallet::{
    address::Network,
    ed25519::{CompressedPoint, Point, Scalar},
};
use zeroize::Zeroizing;

// Same numbering as monero-ts / zebgen's MoneroNetworkType.
pub fn network_from_u8(network: u8) -> Result<Network, String> {
    match network {
        0 => Ok(Network::Mainnet),
        1 => Ok(Network::Testnet),
        2 => Ok(Network::Stagenet),
        other => Err(format!("unknown network type {other}")),
    }
}

fn hex32(label: &str, value: &str) -> Result<Zeroizing<[u8; 32]>, String> {
    let bytes =
        Zeroizing::new(hex::decode(value).map_err(|e| format!("{label}: invalid hex: {e}"))?);
    let array: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("{label}: expected 32 bytes"))?;
    Ok(Zeroizing::new(array))
}

pub fn scalar_from_hex(label: &str, value: &str) -> Result<Zeroizing<Scalar>, String> {
    let bytes = hex32(label, value)?;
    Scalar::read(&mut bytes.as_slice())
        .map(Zeroizing::new)
        .map_err(|_| format!("{label}: not a canonical scalar"))
}

pub fn point_from_hex(label: &str, value: &str) -> Result<Point, String> {
    CompressedPoint::from(*hex32(label, value)?)
        .decompress()
        .ok_or_else(|| format!("{label}: not a valid curve point"))
}

pub fn public_key(scalar: &Scalar) -> Point {
    Point::from(curve25519_dalek::constants::ED25519_BASEPOINT_POINT * scalar.clone().into())
}
