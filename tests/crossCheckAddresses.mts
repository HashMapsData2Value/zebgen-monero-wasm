// Cross-checks the wasm build against zebgen's own TypeScript key/address code.
// Run from this repo: npx --prefix ../leakswap-all/leakswap-private/e2e-ui tsx tests/crossCheckAddresses.ts
import { createRequire } from 'node:module';
import {
  MoneroNetworkType,
  genPrivateSpendKey,
  getMoneroKeyMaterialFromPrivateSpendKey,
} from '../../zebgen/lib/wallet/moneroWallet';

const require = createRequire(import.meta.url);
const wasm = require('../pkg-node/zebgen_monero_wasm.js');

const networks = [MoneroNetworkType.MAINNET, MoneroNetworkType.TESTNET, MoneroNetworkType.STAGENET];
let checked = 0;
for (let i = 0; i < 200; i++) {
  const network = networks[i % networks.length];
  const keys = getMoneroKeyMaterialFromPrivateSpendKey(genPrivateSpendKey(), network);

  const publicSpend = wasm.publicKeyFromPrivate(keys.privateSpendKey);
  const publicView = wasm.publicKeyFromPrivate(keys.privateViewKey);
  const address = wasm.primaryAddress(network, publicSpend, keys.privateViewKey);

  const mismatches = [
    publicSpend !== keys.publicSpendKey && 'publicSpendKey',
    publicView !== keys.publicViewKey && 'publicViewKey',
    address !== keys.address && 'address',
    !wasm.isValidAddress(network, keys.address) && 'isValidAddress',
  ].filter(Boolean);
  if (mismatches.length > 0) {
    console.error('MISMATCH', { network, keys, publicSpend, publicView, address, mismatches });
    process.exit(1);
  }
  checked++;
}
console.log(`OK: ${checked} random key sets match zebgen's TypeScript derivation`);
