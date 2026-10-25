const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const { randomUUID } = require('node:crypto');
const { test } = require('node:test');
const wasm = require('../pkg-node/zebgen_monero_wasm.js');

const docker = (...args) => execFileSync('docker', args, { encoding: 'utf8' }).trim();

test(
  'funded cached payments and sweeps preserve amounts, fees, maturity and spent checks',
  { timeout: 180_000 },
  async () => {
    const name = `zebgen-checkpoint-${randomUUID()}`;
    const walletName = `${name}-wallet`;
    const image = process.env.MONERO_TEST_IMAGE || 'docker-monerod';
    const originalFetch = globalThis.fetch;
    const downloads = [];
    let daemon;
    let walletRpc;
    const rpc = async (url, method, params = {}) => {
      const response = await originalFetch(`${url}/json_rpc`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: '0', method, params }),
      });
      const body = await response.json();
      if (body.error) throw new Error(`${method}: ${JSON.stringify(body.error)}`);
      return body.result;
    };
    const ready = async (url, method) => {
      const deadline = Date.now() + 15_000;
      for (;;) {
        try {
          return await rpc(url, method);
        } catch (error) {
          if (Date.now() >= deadline) throw error;
        }
      }
    };
    const keys = (number) => {
      const spend = number.toString(16).padStart(2, '0') + '00'.repeat(31);
      const view = (number + 10).toString(16).padStart(2, '0') + '00'.repeat(31);
      return {
        spend,
        view,
        address: wasm.primaryAddress(0, wasm.publicKeyFromPrivate(spend), view),
      };
    };
    const sender = keys(1);
    const recipient = keys(2);
    const miner = keys(3);
    const destination = keys(4);
    const amount = 1_000_000_000_000n;
    const scan = async (owner, checkpoint) => {
      for (;;) {
        const result = JSON.parse(
          await wasm.scanBalanceBatch(
            daemon,
            undefined,
            undefined,
            owner.spend,
            owner.view,
            1,
            checkpoint && JSON.stringify(checkpoint),
          ),
        );
        if (result.done) return result;
        checkpoint = result.checkpoint;
      }
    };
    const payment = (owner, checkpoint, target, value) =>
      wasm.createTransaction(
        daemon,
        undefined,
        undefined,
        0,
        owner.spend,
        owner.view,
        1,
        target.address,
        value.toString(),
        false,
        JSON.stringify(checkpoint),
      );
    const mine = (blocks) =>
      rpc(daemon, 'generateblocks', { amount_of_blocks: blocks, wallet_address: miner.address });
    const pool = () =>
      originalFetch(`${daemon}/get_transaction_pool_hashes`, { method: 'POST' }).then((response) =>
        response.json(),
      );
    const independentlyCheckBalance = async (owner, filename, expected) => {
      await rpc(walletRpc, 'generate_from_keys', {
        filename,
        password: '',
        address: owner.address,
        viewkey: owner.view,
        restore_height: 1,
      });
      await rpc(walletRpc, 'refresh', { start_height: 1 });
      const balance = await rpc(walletRpc, 'get_balance');
      assert.equal(BigInt(balance.balance), expected);
      assert.equal(BigInt(balance.unlocked_balance), expected);
      await rpc(walletRpc, 'close_wallet');
    };
    try {
      docker(
        'run',
        '-d',
        '--rm',
        '--pull=never',
        '--name',
        name,
        '-p',
        '127.0.0.1::18081',
        '-p',
        '127.0.0.1::18083',
        image,
        'monerod',
        '--regtest',
        '--offline',
        '--fixed-difficulty=1',
        '--data-dir=/tmp/checkpoint-chain',
        '--non-interactive',
        '--no-igd',
        '--disable-rpc-ban',
        '--rpc-bind-ip=0.0.0.0',
        '--rpc-bind-port=18081',
        '--confirm-external-bind',
      );
      daemon = `http://${docker('port', name, '18081/tcp')}`;
      walletRpc = `http://${docker('port', name, '18083/tcp')}`;
      const info = await ready(daemon, 'get_info');
      assert.equal(info.nettype, 'fakechain');
      assert.ok(info.height <= 2, 'test must start on its own fresh chain');
      docker(
        'run',
        '-d',
        '--rm',
        '--pull=never',
        '--name',
        walletName,
        '--network',
        `container:${name}`,
        image,
        'monero-wallet-rpc',
        '--disable-rpc-login',
        '--wallet-dir=/tmp',
        '--daemon-address=127.0.0.1:18081',
        '--rpc-bind-ip=0.0.0.0',
        '--rpc-bind-port=18083',
        '--confirm-external-bind',
        '--allow-mismatched-daemon-version',
      );
      await ready(walletRpc, 'get_version');
      await rpc(daemon, 'generateblocks', { amount_of_blocks: 1, wallet_address: sender.address });
      await mine(200);
      const initial = await scan(sender);
      assert.ok(BigInt(initial.balanceAtomic) > amount);
      globalThis.fetch = async (input, options) => {
        const url = typeof input === 'string' ? input : input.url;
        if (url.includes('get_blocks')) downloads.push(url);
        return originalFetch(input, options);
      };
      const transaction = await payment(sender, initial.checkpoint, recipient, amount);
      assert.equal(downloads.length, 0, 'funded warm preparation must not fetch historical blocks');
      assert.equal(transaction.amountAtomic, amount.toString());
      assert.equal(transaction.balanceAtomic, initial.balanceAtomic);
      assert.ok(BigInt(transaction.feeAtomic) > 0n);
      assert.equal(transaction.relayed, false);
      assert.ok(transaction.checkpoint);
      assert.ok(!(await pool()).tx_hashes?.includes(transaction.hash));
      assert.equal(
        await wasm.relayTransaction(daemon, undefined, undefined, transaction.txHex),
        transaction.hash,
      );
      assert.ok((await pool()).tx_hashes.includes(transaction.hash));
      await assert.rejects(
        payment(sender, initial.checkpoint, recipient, amount),
        /insufficient unlocked funds/,
      );
      await mine(1);
      await assert.rejects(
        payment(sender, initial.checkpoint, recipient, amount),
        /insufficient unlocked funds/,
      );
      const locked = await scan(recipient);
      assert.equal(locked.balanceAtomic, '0');
      assert.equal(locked.lockedAtomic, amount.toString());
      await assert.rejects(
        payment(recipient, locked.checkpoint, destination, 1n),
        /insufficient unlocked funds/,
      );
      await mine(10);
      const senderAfter = await scan(sender, JSON.parse(transaction.checkpoint));
      assert.equal(
        BigInt(senderAfter.balanceAtomic),
        BigInt(initial.balanceAtomic) - amount - BigInt(transaction.feeAtomic),
      );
      await independentlyCheckBalance(recipient, 'recipient', amount);
      const sweep = await wasm.sweepTransaction(
        daemon,
        undefined,
        undefined,
        0,
        recipient.spend,
        recipient.view,
        1,
        destination.address,
        false,
        JSON.stringify(locked.checkpoint),
      );
      assert.equal(BigInt(sweep.balanceAtomic), amount);
      assert.equal(BigInt(sweep.amountAtomic) + BigInt(sweep.feeAtomic), amount);
      assert.ok(sweep.checkpoint);
      assert.equal(sweep.relayed, false);
      await wasm.relayTransaction(daemon, undefined, undefined, sweep.txHex);
      await assert.rejects(
        wasm.sweepTransaction(
          daemon,
          undefined,
          undefined,
          0,
          recipient.spend,
          recipient.view,
          1,
          destination.address,
          false,
          JSON.stringify(locked.checkpoint),
        ),
        /no unlocked funds/,
      );
      await mine(11);
      const swept = await scan(recipient, JSON.parse(sweep.checkpoint));
      assert.equal(swept.balanceAtomic, '0');
      await independentlyCheckBalance(destination, 'destination', BigInt(sweep.amountAtomic));
    } finally {
      globalThis.fetch = originalFetch;
      for (const container of [walletName, name]) {
        try {
          docker('rm', '-f', container);
        } catch (error) {
          if (!String(error.stderr).includes('No such container')) throw error;
        }
      }
    }
  },
);
