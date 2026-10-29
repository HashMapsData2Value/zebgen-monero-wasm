const assert = require('node:assert/strict');
const { execFileSync, spawnSync } = require('node:child_process');
const { randomUUID } = require('node:crypto');
const path = require('node:path');
const wasm = require('../pkg-node/zebgen_monero_wasm.js');

const root = path.resolve(__dirname, '..');
const docker = (...args) => execFileSync('docker', args, { encoding: 'utf8' }).trim();
const name = `zebgen-test-audit-${randomUUID()}`;
const walletName = `${name}-wallet`;
const image = process.env.MONERO_TEST_IMAGE ?? 'docker-monerod';
const files = process.argv.slice(2);
const tests = files.length
  ? files
  : [
      'balanceBatch.cjs',
      'balanceCancellation.cjs',
      'scanCoverage.cjs',
      'transactionCheckpoint.cjs',
      'regtestFundingControl.mts',
      'regtestSend.mts',
      'authRestrictedSend.mts',
    ];

async function rpc(url, method, params = {}) {
  const response = await fetch(`${url}/json_rpc`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: '0', method, params }),
    signal: AbortSignal.timeout(method === 'generateblocks' ? 120_000 : 2_000),
  });
  const body = await response.json();
  if (body.error) throw new Error(`${method}: ${JSON.stringify(body.error)}`);
  return body.result;
}

async function ready(url, method) {
  const deadline = Date.now() + 60_000;
  for (;;) {
    try {
      return await rpc(url, method);
    } catch (error) {
      if (Date.now() >= deadline) throw error;
    }
  }
}

async function main() {
  try {
    docker(
      'run',
      '-d',
      '--pull=never',
      '--name',
      name,
      '-p',
      '127.0.0.1::18081',
      '-p',
      '127.0.0.1::18083',
      '-p',
      '127.0.0.1::28089',
      image,
      'monerod',
      '--regtest',
      '--offline',
      '--fixed-difficulty=1',
      '--data-dir=/tmp/test-chain',
      '--non-interactive',
      '--no-igd',
      '--disable-rpc-ban',
      '--rpc-bind-ip=0.0.0.0',
      '--rpc-bind-port=18081',
      '--rpc-restricted-bind-ip=0.0.0.0',
      '--rpc-restricted-bind-port=28089',
      '--confirm-external-bind',
    );
    const daemon = `http://${docker('port', name, '18081/tcp')}`;
    const wallet = `http://${docker('port', name, '18083/tcp')}`;
    const restricted = `http://${docker('port', name, '28089/tcp')}`;
    const info = await ready(daemon, 'get_info');
    assert.equal(info.nettype, 'fakechain');
    assert.ok(info.height <= 2, 'isolated tests must start on a fresh chain');
    docker(
      'run',
      '-d',
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
    await ready(wallet, 'get_version');
    await ready(restricted, 'get_info');
    const spend = '31' + '00'.repeat(31);
    const view = '32' + '00'.repeat(31);
    for (let blocks = 0; blocks < 1_400; blocks += 100) {
      await rpc(daemon, 'generateblocks', {
        amount_of_blocks: 100,
        wallet_address: wasm.primaryAddress(0, wasm.publicKeyFromPrivate(spend), view),
      });
      console.log(`Seeded ${blocks + 100}/1400 isolated blocks`);
    }
    const env = {
      ...process.env,
      MONERO_TEST_DAEMON: daemon,
      MONERO_TEST_WALLET_RPC: wallet,
      MONERO_TEST_RESTRICTED: restricted,
    };
    let failed = 0;
    for (const file of tests) {
      const command = file.endsWith('.mts')
        ? (process.env.TSX ??
          path.resolve(root, '../leakswap-all/leakswap-private/e2e-ui/node_modules/.bin/tsx'))
        : process.execPath;
      const args = file.endsWith('.cjs') ? ['--test', `tests/${file}`] : [`tests/${file}`];
      console.log(`RUN ${command} ${args.join(' ')}`);
      const result = spawnSync(command, args, {
        cwd: root,
        env,
        stdio: 'inherit',
        timeout: 180_000,
      });
      if (result.error) console.error(result.error);
      const passed = result.status === 0;
      console.log(`${passed ? 'PASS' : 'FAIL'} ${file}`);
      if (!passed) failed += 1;
    }
    console.log(`Isolated files: ${tests.length - failed} passed, ${failed} failed`);
    process.exitCode = failed ? 1 : 0;
  } catch (error) {
    for (const container of [name, walletName]) {
      try {
        console.error(docker('logs', container));
      } catch (logError) {
        console.error(logError);
      }
    }
    throw error;
  } finally {
    for (const container of [walletName, name]) {
      try {
        docker('rm', '-f', container);
      } catch (error) {
        if (!String(error.stderr).includes('No such container')) throw error;
      }
    }
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
