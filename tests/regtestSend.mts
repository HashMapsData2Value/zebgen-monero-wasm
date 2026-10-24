// End-to-end send on the docker regtest chain using only the wasm build for wallet logic.
// The recipient side is independently verified by Monero's own wallet2 (monero-wallet-rpc).
// Requires leakswap-private/docker running. Run from this repo:
//   ../leakswap-all/leakswap-private/e2e-ui/node_modules/.bin/tsx tests/regtestSend.ts
import { createRequire } from 'node:module';
import {
  MoneroNetworkType,
  genPrivateSpendKey,
  getMoneroKeyMaterialFromPrivateSpendKey,
} from '../../zebgen/lib/wallet/moneroWallet';

const require = createRequire(import.meta.url);
const wasm = require('../pkg-node/zebgen_monero_wasm.js');

const DAEMON = 'http://127.0.0.1:18081';
const WALLET_RPC = 'http://127.0.0.1:18083';
const ONE_XMR = 1_000_000_000_000n;

async function jsonRpc<T>(url: string, method: string, params: object = {}): Promise<T> {
  const response = await fetch(`${url}/json_rpc`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 0, method, params }),
  });
  const body = await response.json();
  if (body.error) throw new Error(`${method}: ${JSON.stringify(body.error)}`);
  return body.result as T;
}

const mine = (address: string, blocks: number) =>
  jsonRpc(DAEMON, 'generateblocks', { amount_of_blocks: blocks, wallet_address: address });

const newKeys = () =>
  getMoneroKeyMaterialFromPrivateSpendKey(genPrivateSpendKey(), MoneroNetworkType.MAINNET);

function expect(condition: boolean, message: string): void {
  if (!condition) throw new Error(`FAILED: ${message}`);
  console.log(`ok - ${message}`);
}

const networkInfo = await jsonRpc<{ nettype: string }>(DAEMON, 'get_info');
expect(
  networkInfo.nettype === 'fakechain',
  'daemon is the isolated local regtest chain, not public mainnet',
);
console.log(
  `LOCAL REGTEST ONLY at ${DAEMON}: locally mined test coins; mainnet address encoding does not mean public mainnet.`,
);

const sender = newKeys();
const recipient = newKeys();
const miner = newKeys();

const restoreHeight: number = await wasm.daemonHeight(DAEMON);
console.log(`daemon height ${restoreHeight}`);

await mine(sender.address, 1);
await mine(miner.address, 70);

const startBalance = BigInt(
  await wasm.unlockedBalance(
    DAEMON,
    undefined,
    undefined,
    sender.privateSpendKey,
    sender.privateViewKey,
    restoreHeight,
  ),
);
expect(startBalance > ONE_XMR, `sender's mined coinbase is found and unlocked (${startBalance})`);

const t0 = Date.now();
const tx = await wasm.createTransaction(
  DAEMON,
  undefined,
  undefined,
  MoneroNetworkType.MAINNET,
  sender.privateSpendKey,
  sender.privateViewKey,
  restoreHeight,
  recipient.address,
  ONE_XMR.toString(),
  false,
);
console.log(`createTransaction took ${Date.now() - t0}ms`, {
  ...tx,
  txHex: `${tx.txHex.length / 2} bytes`,
});
expect(BigInt(tx.balanceAtomic) === startBalance, 'reported balance matches unlockedBalance');

const beforeRelay = await fetch(`${DAEMON}/get_transaction_pool_hashes`, { method: 'POST' }).then(
  (response) => response.json(),
);
expect(
  !tx.relayed && !beforeRelay.tx_hashes?.includes(tx.hash),
  'prepared payment is not broadcast',
);
const relayedHash = await wasm.relayTransaction(DAEMON, undefined, undefined, tx.txHex);
expect(relayedHash === tx.hash, 'relay publishes the exact prepared payment hash');

const pool = await fetch(`${DAEMON}/get_transaction_pool_hashes`, { method: 'POST' }).then((r) =>
  r.json(),
);
expect(pool.tx_hashes?.includes(tx.hash), 'daemon accepted the transaction into its mempool');

await mine(miner.address, 1);
const lockedFunding = await wasm.scanFunding(
  DAEMON,
  undefined,
  undefined,
  recipient.publicSpendKey,
  recipient.privateViewKey,
  restoreHeight,
);
expect(
  lockedFunding.receivedAtomic === ONE_XMR.toString() &&
    lockedFunding.matureAtomic === '0' &&
    lockedFunding.lockedAtomic === ONE_XMR.toString() &&
    lockedFunding.spentStatusKnown === false,
  'view-only scan detects locked funding without claiming spent knowledge',
);
const lockedBatch = JSON.parse(
  await wasm.scanBalanceBatch(
    DAEMON,
    undefined,
    undefined,
    recipient.privateSpendKey,
    recipient.privateViewKey,
    restoreHeight,
  ),
);
expect(
  lockedBatch.done &&
    lockedBatch.balanceAtomic === '0' &&
    lockedBatch.lockedAtomic === ONE_XMR.toString(),
  'normal-wallet scanner reports mined unspent payment as locked',
);
await mine(miner.address, 10);
const matureBatch = JSON.parse(
  await wasm.scanBalanceBatch(
    DAEMON,
    undefined,
    undefined,
    recipient.privateSpendKey,
    recipient.privateViewKey,
    restoreHeight,
    JSON.stringify(lockedBatch.checkpoint),
  ),
);
expect(
  matureBatch.done &&
    matureBatch.balanceAtomic === ONE_XMR.toString() &&
    matureBatch.lockedAtomic === '0',
  'cached payment becomes spendable when blocks advance',
);

const recipientBalance = BigInt(
  await wasm.unlockedBalance(
    DAEMON,
    undefined,
    undefined,
    recipient.privateSpendKey,
    recipient.privateViewKey,
    restoreHeight,
  ),
);
expect(
  recipientBalance === ONE_XMR,
  `wasm sees exactly 1 XMR at the recipient (${recipientBalance})`,
);
const matureFunding = await wasm.scanFunding(
  DAEMON,
  undefined,
  undefined,
  recipient.publicSpendKey,
  recipient.privateViewKey,
  restoreHeight,
);
expect(
  matureFunding.receivedAtomic === ONE_XMR.toString() &&
    matureFunding.matureAtomic === ONE_XMR.toString() &&
    matureFunding.lockedAtomic === '0',
  'view-only scan detects maturity without a private spend key',
);

const senderBalance = BigInt(
  await wasm.unlockedBalance(
    DAEMON,
    undefined,
    undefined,
    sender.privateSpendKey,
    sender.privateViewKey,
    restoreHeight,
  ),
);
expect(
  senderBalance === startBalance - ONE_XMR - BigInt(tx.feeAtomic),
  `sender change is spendable and the spent input is excluded (${senderBalance})`,
);

const filename = `wasm-check-${Date.now()}`;
await jsonRpc(WALLET_RPC, 'generate_from_keys', {
  filename,
  password: '',
  address: recipient.address,
  spendkey: recipient.privateSpendKey,
  viewkey: recipient.privateViewKey,
  restore_height: restoreHeight,
});
await jsonRpc(WALLET_RPC, 'refresh', { start_height: restoreHeight });
const walletBalance = await jsonRpc<{ unlocked_balance: number }>(WALLET_RPC, 'get_balance', {});
await jsonRpc(WALLET_RPC, 'close_wallet', {});
expect(
  BigInt(walletBalance.unlocked_balance) === ONE_XMR,
  `Monero's own wallet2 (monero-wallet-rpc) independently sees 1 XMR at the recipient`,
);

const final = newKeys();
const sweep = await wasm.sweepTransaction(
  DAEMON,
  undefined,
  undefined,
  MoneroNetworkType.MAINNET,
  recipient.privateSpendKey,
  recipient.privateViewKey,
  restoreHeight,
  final.address,
  false,
);
console.log('sweepTransaction', { ...sweep, txHex: `${sweep.txHex.length / 2} bytes` });
const beforeSweepRelay = await fetch(`${DAEMON}/get_transaction_pool_hashes`, {
  method: 'POST',
}).then((response) => response.json());
expect(
  !sweep.relayed && !beforeSweepRelay.tx_hashes?.includes(sweep.hash),
  'prepared sweep is not broadcast',
);
expect(
  (await wasm.relayTransaction(DAEMON, undefined, undefined, sweep.txHex)) === sweep.hash,
  'relay publishes the exact prepared sweep hash',
);
expect(
  BigInt(sweep.amountAtomic) + BigInt(sweep.feeAtomic) === ONE_XMR,
  `sweep sends the whole balance minus the fee (${sweep.amountAtomic} + ${sweep.feeAtomic})`,
);

await mine(miner.address, 11);
const sweptBalance = BigInt(
  await wasm.unlockedBalance(
    DAEMON,
    undefined,
    undefined,
    recipient.privateSpendKey,
    recipient.privateViewKey,
    restoreHeight,
  ),
);
expect(sweptBalance === 0n, `swept wallet is empty afterwards (${sweptBalance})`);
const spentBatch = JSON.parse(
  await wasm.scanBalanceBatch(
    DAEMON,
    undefined,
    undefined,
    recipient.privateSpendKey,
    recipient.privateViewKey,
    restoreHeight,
    JSON.stringify(matureBatch.checkpoint),
  ),
);
expect(
  spentBatch.done && spentBatch.balanceAtomic === '0' && spentBatch.lockedAtomic === '0',
  'normal-wallet totals exclude spent outputs retained in the checkpoint',
);
const spentFunding = await wasm.scanFunding(
  DAEMON,
  undefined,
  undefined,
  recipient.publicSpendKey,
  recipient.privateViewKey,
  restoreHeight,
);
expect(
  spentFunding.receivedAtomic === ONE_XMR.toString() &&
    spentFunding.matureAtomic === ONE_XMR.toString() &&
    spentFunding.spentStatusKnown === false,
  'historical funding remains visible after spending; it is not an unspent balance',
);

await jsonRpc(WALLET_RPC, 'generate_from_keys', {
  filename: `wasm-sweep-check-${Date.now()}`,
  password: '',
  address: final.address,
  viewkey: final.privateViewKey,
  restore_height: restoreHeight,
});
await jsonRpc(WALLET_RPC, 'refresh', { start_height: restoreHeight });
const finalBalance = await jsonRpc<{ unlocked_balance: number }>(WALLET_RPC, 'get_balance', {});
await jsonRpc(WALLET_RPC, 'close_wallet', {});
expect(
  BigInt(finalBalance.unlocked_balance) === BigInt(sweep.amountAtomic),
  `Monero's own wallet2 sees the swept amount at the final address (view-only)`,
);

console.log('PASS');
