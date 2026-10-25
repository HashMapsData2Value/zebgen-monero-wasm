const assert = require('node:assert/strict');
const { test } = require('node:test');
const wasm = require('../pkg-node/zebgen_monero_wasm.js');

test('transaction preparation reuses coverage, scans deltas and rejects another wallet without relay', async () => {
  const daemon = 'http://127.0.0.1:18081';
  const spend = '01' + '00'.repeat(31);
  const view = '02' + '00'.repeat(31);
  const recipient = wasm.primaryAddress(0, wasm.publicKeyFromPrivate(spend), view);
  const { height } = await (await fetch(`${daemon}/get_height`)).json();
  const restore = height - 5;
  const snapshot = JSON.parse(
    await wasm.scanBalanceBatch(daemon, undefined, undefined, spend, view, restore),
  );
  assert.equal(snapshot.done, true);
  const originalFetch = globalThis.fetch;
  const blocks = [];
  globalThis.fetch = async (input, options) => {
    const url = typeof input === 'string' ? input : input.url;
    assert.doesNotMatch(url, /send_raw_transaction/);
    if (url.includes('get_blocks')) blocks.push(url);
    return originalFetch(input, options);
  };
  const prepare = (checkpoint) =>
    wasm.createTransaction(
      daemon,
      undefined,
      undefined,
      0,
      spend,
      view,
      restore,
      recipient,
      '1',
      false,
      JSON.stringify(checkpoint),
    );
  try {
    assert.equal(wasm.transactionCheckpointVersion(), 2);
    await assert.rejects(prepare(snapshot.checkpoint), /insufficient unlocked funds/);
    assert.equal(blocks.length, 0, 'current coverage must not download historical blocks');
    await assert.rejects(
      wasm.sweepTransaction(
        daemon,
        undefined,
        undefined,
        0,
        spend,
        view,
        restore,
        recipient,
        false,
        JSON.stringify(snapshot.checkpoint),
      ),
      /no unlocked funds/,
    );
    assert.equal(blocks.length, 0, 'sweep must reuse the same coverage');
    const end = height - 2;
    const header = await (
      await originalFetch(`${daemon}/json_rpc`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          jsonrpc: '2.0',
          id: '0',
          method: 'get_block_header_by_height',
          params: { height: end - 1 },
        }),
      })
    ).json();
    const partial = {
      ...snapshot.checkpoint,
      spans: [{ start: restore, end, anchorHash: header.result.block_header.hash }],
    };
    await assert.rejects(prepare(partial), /insufficient unlocked funds/);
    assert.ok(blocks.length > 0, 'missing delta must be downloaded');
    blocks.length = 0;
    await assert.rejects(
      prepare({ ...snapshot.checkpoint, publicSpendKey: '00'.repeat(32) }),
      /another wallet/,
    );
    assert.equal(blocks.length, 0);
    await assert.rejects(
      prepare({
        ...snapshot.checkpoint,
        spans: [{ ...snapshot.checkpoint.spans[0], anchorHash: '00'.repeat(32) }],
      }),
      /insufficient unlocked funds/,
    );
    assert.ok(blocks.length > 0, 'invalid chain anchor must discard stale coverage');
  } finally {
    globalThis.fetch = originalFetch;
  }
});
