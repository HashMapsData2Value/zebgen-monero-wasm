const assert = require('node:assert/strict');
const { test } = require('node:test');
const { scanBalanceBatch } = require('../pkg-node/zebgen_monero_wasm.js');

test('disjoint scans preserve gaps, fill only missing blocks, and refresh covered history without downloads', async () => {
  const daemon = process.env.MONERO_TEST_DAEMON ?? 'http://127.0.0.1:18081';
  const spend = '01' + '00'.repeat(31);
  const view = '02' + '00'.repeat(31);
  const originalFetch = globalThis.fetch;
  let blockDownloads = 0;
  globalThis.fetch = (...args) => {
    if (String(args[0]?.url ?? args[0]).includes('get_blocks')) blockDownloads++;
    return originalFetch(...args);
  };
  try {
    let checkpoint;
    const scan = async (start) => {
      const result = JSON.parse(
        await scanBalanceBatch(
          daemon,
          undefined,
          undefined,
          spend,
          view,
          start,
          checkpoint && JSON.stringify(checkpoint),
        ),
      );
      checkpoint = result.checkpoint;
      return result;
    };
    const spans = () => checkpoint.spans.map(({ start, end }) => [start, end]);
    await scan(1000);
    assert.deepEqual(spans(), [[1000, 1100]]);
    const { height } = await (await originalFetch(`${daemon}/get_height`)).json();
    assert.ok(height > 1300, 'test daemon needs at least 1301 blocks');
    await scan(height - 100);
    assert.deepEqual(spans(), [
      [1000, 1100],
      [height - 100, height],
    ]);
    let result;
    do {
      const before = blockDownloads;
      result = await scan(1000);
      assert.equal(
        blockDownloads - before,
        1,
        'each batch downloads exactly one uncovered block interval',
      );
    } while (!result.done);
    assert.deepEqual(spans(), [[1000, height]]);
    const before = blockDownloads;
    result = await scan(1200);
    assert.equal(result.done, true);
    assert.equal(blockDownloads, before, 'already covered history must not be downloaded again');
    assert.equal(result.lockedAtomic, '0');
  } finally {
    globalThis.fetch = originalFetch;
  }
});
