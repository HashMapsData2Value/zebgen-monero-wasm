// --restricted-rpc check against a throwaway monerod:
//   docker run -d --rm --name zebgen-auth-test-monerod -p 127.0.0.1:28081:28081 -p 127.0.0.1:28089:28089 \
//     docker-monerod monerod --regtest --offline --fixed-difficulty=1 --data-dir=/tmp/regtest \
//     --non-interactive --no-igd --disable-rpc-ban --rpc-bind-ip=0.0.0.0 --rpc-bind-port=28081 \
//     --rpc-restricted-bind-ip=0.0.0.0 --rpc-restricted-bind-port=28089 --confirm-external-bind
// Digest login (--rpc-login) is covered in Chrome by leakswap-private/e2e-ui/validateDaemonLogin.ts:
// monerod binds each nonce to its TCP connection and Node's fetch never reuses connections.
// Run from this repo:
//   ../leakswap-all/leakswap-private/e2e-ui/node_modules/.bin/tsx tests/authRestrictedSend.mts
import { createRequire } from 'node:module';
import {
  MoneroNetworkType,
  genPrivateSpendKey,
  getMoneroKeyMaterialFromPrivateSpendKey,
} from '../../zebgen/lib/wallet/moneroWallet';

const require = createRequire(import.meta.url);
const wasm = require('../pkg-node/zebgen_monero_wasm.js');

const RESTRICTED = process.env.MONERO_TEST_RESTRICTED ?? 'http://127.0.0.1:28089';
const ADMIN = process.env.MONERO_TEST_DAEMON ?? 'http://127.0.0.1:28081';
const ONE_XMR = 1_000_000_000_000n;

function expect(condition: boolean, message: string): void {
  if (!condition) throw new Error(`FAILED: ${message}`);
  console.log(`ok - ${message}`);
}

async function mine(address: string, blocks: number) {
  const response = await fetch(`${ADMIN}/json_rpc`, {
    method: 'POST',
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 0,
      method: 'generateblocks',
      params: { amount_of_blocks: blocks, wallet_address: address },
    }),
  });
  const parsed = await response.json();
  if (parsed.error) throw new Error(`generateblocks: ${JSON.stringify(parsed.error)}`);
}

const newKeys = () =>
  getMoneroKeyMaterialFromPrivateSpendKey(genPrivateSpendKey(), MoneroNetworkType.MAINNET);

const restoreHeight: number = await wasm.daemonHeight(RESTRICTED);
expect(restoreHeight >= 1, `restricted port reachable (height ${restoreHeight})`);

const sender = newKeys();
const recipient = newKeys();
const miner = newKeys();
await mine(sender.address, 1);
// A fresh chain needs enough RingCT outputs for 16-member rings.
await mine(miner.address, 200);

const startBalance = BigInt(
  await wasm.unlockedBalance(RESTRICTED, undefined, undefined, sender.privateSpendKey, sender.privateViewKey, restoreHeight)
);
expect(startBalance > ONE_XMR, `restricted RPC: scan + spent check work (${startBalance})`);

const tx = await wasm.createTransaction(
  RESTRICTED,
  undefined,
  undefined,
  MoneroNetworkType.MAINNET,
  sender.privateSpendKey,
  sender.privateViewKey,
  restoreHeight,
  recipient.address,
  ONE_XMR.toString(),
  true
);
expect(Boolean(tx.hash) && tx.relayed, `restricted RPC: decoys, fee, sign and relay work (${tx.hash})`);

await mine(miner.address, 11);
const recipientBalance = BigInt(
  await wasm.unlockedBalance(
    RESTRICTED,
    undefined,
    undefined,
    recipient.privateSpendKey,
    recipient.privateViewKey,
    restoreHeight
  )
);
expect(recipientBalance === ONE_XMR, `recipient received exactly 1 XMR (${recipientBalance})`);
console.log('PASS');
