# zebgen-monero-wasm

Monero wallet functions for browsers and browser extensions, compiled to
WebAssembly from Rust. It wraps the [monero-oxide](https://github.com/monero-oxide/monero-oxide)
crates [`monero-wallet`](https://crates.io/crates/monero-wallet) and
[`monero-daemon-rpc`](https://crates.io/crates/monero-daemon-rpc) (both pinned to exactly
`0.2.0`), and is used by the [zebgen](https://github.com/HashMapsData2Value/zebgen) wallet
extension.

It scans for a wallet's funds and builds and signs transactions **locally**. A Monero node
only supplies blockchain data and broadcasts the finished, signed transaction.

## Why this exists

zebgen originally used [`monero-ts`](https://github.com/woodser/monero-ts), which ran into three
problems in an MV3 browser extension: it uses `eval` (forbidden by the extension's security
policy), its HTTP client broke when bundled, and `wallet.sync()` crashed inside its prebuilt
WebAssembly (`Exception in thread pool`, also reproducible in plain Node.js). See
[zebgen's README](https://github.com/HashMapsData2Value/zebgen#monero-why-zebgen-monero-wasm-instead-of-monero-ts)
for the full story.

This package is built from source with no prebuilt binaries and needs no `eval` or Web Workers.
It's tested in Chrome's MV3 extension service worker and in Node.js (via the CommonJS build).

## Security model

```
your app / extension (keys never leave)          any Monero node
┌─────────────────────────────────────┐         ┌───────────────────────┐
│ zebgen-monero-wasm                  │ blocks, │ your own monerod,     │
│   scan (private view key)           │◄────────┤ a public node,        │
│   check spent (key images)          │ decoys, │ a hosted node API     │
│   pick coins + decoys, sign         │ fees    │                       │
│   (private spend key)               ├────────►│ broadcasts the tx     │
└─────────────────────────────────────┘ signed  └───────────────────────┘
                                          tx only
```

- **Keys never leave the caller.** Whole blocks are downloaded and scanned locally, so the node
  never receives the view key (unlike "light wallet" servers such as monero-lws).
- **The node can't steal funds**, but it does affect privacy: it sees your IP address and the
  transaction you broadcast, and it supplies the decoy outputs used in your ring. A malicious
  node could pick decoys badly. Use a node you trust, ideally your own.
- Fee rates suggested by the node are capped at 10,000,000 atomic units per weight unit.
  Response sizes are bounded by `monero-daemon-rpc` to limit damage from hostile nodes.
- Parsed private keys and daemon passwords are held in types that wipe memory on drop
  (`zeroize`). The hex strings passed in from JavaScript, and Rust's copies of them, are not
  wiped.
- The build fails if the generated JavaScript ever contains `eval` or `new Function`.

This wrapper has **not** been audited. monero-oxide publishes a third-party audit in its own
repository.

## Install

Releases are built from the tagged source by GitHub Actions and attached to
[GitHub Releases](https://github.com/HashMapsData2Value/zebgen-monero-wasm/releases) as an npm
tarball, with a `.sha256` file alongside.

```sh
npm install --save-exact https://github.com/HashMapsData2Value/zebgen-monero-wasm/releases/download/v0.1.0/zebgen-monero-wasm-0.1.0.tgz
```

`package-lock.json` records the tarball's integrity hash, so later installs fail if the file
changes.

## Usage

The package is an ES module built with `wasm-bindgen --target web`. Call the default export
once to load the WebAssembly before using any other function.

```ts
import init, { daemonHeight, createTransaction } from 'zebgen-monero-wasm';
import wasmUrl from 'zebgen-monero-wasm/zebgen_monero_wasm_bg.wasm?url'; // Vite

// Pass the URL explicitly: bundled service workers have no usable import.meta.url.
await init({ module_or_path: wasmUrl });

const height = await daemonHeight('http://127.0.0.1:18081');

const tx = await createTransaction(
  'http://127.0.0.1:18081',
  undefined,                  // daemon username (optional)
  undefined,                  // daemon password (optional)
  0,                          // network: 0 mainnet, 1 testnet, 2 stagenet
  privateSpendKeyHex,
  privateViewKeyHex,
  restoreHeight,              // first block to scan
  recipientAddress,
  '1000000000000',            // 1 XMR, in atomic units
  true                        // relay: false builds and signs without broadcasting
);
// tx: { hash, feeAtomic, amountAtomic, balanceAtomic, relayed, txHex }
```

In a browser extension, the extension's content security policy must allow WebAssembly
(`script-src 'self' 'wasm-unsafe-eval'`). The extension also needs host permission for the
node's URL.

### API

Keys are 32-byte hex strings. `network` is `0` mainnet, `1` testnet, `2` stagenet (regtest
uses mainnet addresses). Amounts are decimal strings of atomic units (1 XMR = 10¹² atomic
units).

| Function | Returns | Network access |
|---|---|---|
| `publicKeyFromPrivate(privateKeyHex)` | public key hex | no |
| `primaryAddress(network, publicSpendKeyHex, privateViewKeyHex)` | primary address | no |
| `isValidAddress(network, address)` | `boolean` | no |
| `daemonHeight(url, username?, password?)` | chain height (number of blocks) | yes |
| `unlockedBalance(url, username?, password?, privateSpendKeyHex, privateViewKeyHex, restoreHeight)` | spendable balance (string) | yes |
| `createTransaction(url, username?, password?, network, privateSpendKeyHex, privateViewKeyHex, restoreHeight, recipient, amountAtomic, relay)` | `{ hash, feeAtomic, amountAtomic, balanceAtomic, relayed, txHex }` | yes |

`unlockedBalance` needs the private spend key because spent outputs are detected by computing
their key images.

### How a send works

1. Scan blocks from `restoreHeight` to the chain tip, 100 at a time, for outputs to the
   wallet's primary address.
2. Keep only unlocked outputs: at least 10 blocks old, and past the 60-block lock for mined
   (coinbase) outputs.
3. Drop outputs that are already spent, on chain or in the node's mempool, using the node's
   `is_key_image_spent`.
4. Pick outputs largest first until the amount plus fee is covered, and select 15 decoys for
   each (ring size 16).
5. Build a CLSAG + Bulletproofs+ transaction at the node's "normal" fee priority, with change
   going back to the primary address, sign it, and broadcast it if `relay` is true.

### Nodes

- Any `http(s)` node URL works, including nodes running `--restricted-rpc`, the usual mode for
  public nodes.
- **Logins:** HTTP Digest (monerod's `--rpc-login`) and HTTP Basic (typical for a reverse proxy)
  are supported. monerod only accepts a Digest answer on the network connection that issued
  the challenge. Browsers like Chrome reuse connections, so it works there; Node.js's `fetch`
  does not, so Digest logins to monerod fail in Node. Over the internet, use HTTPS.
- Scanning cost grows with the number of blocks scanned. Use a recent `restoreHeight`, such as
  the height when the wallet was created; `0` on mainnet means downloading the whole chain.

## Limitations

- **monero-oxide is young.** It's a 0.x Rust reimplementation of Monero, with far less
  real-world use than Monero's reference wallet (`wallet2`).
- Only the primary address is scanned and used for change; subaddresses are not supported yet.
- Only RingCT outputs can be spent. Pre-RingCT outputs aren't supported; new wallets won't
  have any.
- A node may be able to tell the client is monero-oxide from how it requests data, although
  the transactions are built to match `wallet2`'s.
- Spent detection uses `MoneroDaemon::rpc_call`, which `monero-daemon-rpc` marks as outside its
  SemVer guarantees. It's safe here only because the dependency is pinned to an exact version.

## Building

Requirements:
- Rust 1.89 or newer (CI uses 1.93.0) with the `wasm32-unknown-unknown` target.
- `wasm-bindgen-cli` at **exactly** the `wasm-bindgen` version in `Cargo.lock` (currently
  0.2.129).

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
./scripts/build.sh
```

`scripts/build.sh` writes:
- `pkg/`: the ES-module npm package (`--target web`) with a generated `package.json`.
- `pkg-node/`: a CommonJS build used by the tests.

## Testing

```sh
cargo test
```

Unit tests cover address derivation and the login code, including the HTTP Digest example from
RFC 2617.

The tests in `tests/` run against a real Monero regtest network and need extra setup:
- [`tsx`](https://github.com/privatenumber/tsx) to run them.
- A checkout of zebgen next to this repo (`../zebgen`), since they use its key code.
- For the send tests: `monerod --regtest` with unrestricted RPC at `127.0.0.1:18081`, and
  `monero-wallet-rpc` (`--disable-rpc-login`) at `127.0.0.1:18083`.

| Test | What it checks |
|---|---|
| `tests/crossCheckAddresses.mts` | 200 random key sets give identical public keys and addresses in this package and zebgen's TypeScript code, on all three networks. No node needed. |
| `tests/regtestSend.mts` | Mines coins, scans, sends 1 XMR, and checks the sender's change. The recipient's balance is confirmed independently by Monero's own wallet (`monero-wallet-rpc`). |
| `tests/authRestrictedSend.mts` | The full send through a `--restricted-rpc` port, against a throwaway `monerod` (command in the file's header). |

## Releasing

1. Bump `version` in `Cargo.toml` and commit.
2. Tag and push: `git tag v0.2.0 && git push origin v0.2.0`.

The [release workflow](.github/workflows/release.yml) checks that the tag matches `Cargo.toml`,
runs `cargo test`, builds `pkg/`, and publishes `zebgen-monero-wasm-<version>.tgz` and its
`.sha256` to a GitHub Release.

## License

[MIT](LICENSE)
