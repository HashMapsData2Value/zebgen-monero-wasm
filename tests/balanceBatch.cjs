const assert = require('node:assert/strict');
const { test } = require('node:test');
const { scanBalanceBatch } = require('../pkg-node/zebgen_monero_wasm.js');

const daemon = 'http://127.0.0.1:18081';
const spend = '01' + '00'.repeat(31);
const view = '02' + '00'.repeat(31);

test('bounded scan resumes across restore heights and rejects a different wallet', async () => {
  const first = JSON.parse(await scanBalanceBatch(daemon, undefined, undefined, spend, view, 1));
  assert.equal(first.scannedHeight, 101);
  assert.equal(first.done, false);
  assert.equal(first.checkpoint.outputs.length, 0);
  const second = JSON.parse(
    await scanBalanceBatch(
      daemon,
      undefined,
      undefined,
      spend,
      view,
      1,
      JSON.stringify(first.checkpoint),
    ),
  );
  assert.equal(second.scannedHeight, 201);
  assert.equal(second.checkpoint.restoreHeight, 1);
  const legacy = {
    ...second.checkpoint,
    version: 1,
    anchorHash: second.checkpoint.spans[0].anchorHash,
  };
  delete legacy.spans;
  const adopted = JSON.parse(
    await scanBalanceBatch(daemon, undefined, undefined, spend, view, 2, JSON.stringify(legacy)),
  );
  assert.equal(adopted.checkpoint.version, 2);
  assert.equal(adopted.scannedHeight, 301);
  assert.equal(adopted.checkpoint.spans[0].start, 1);
  await assert.rejects(
    scanBalanceBatch(
      daemon,
      undefined,
      undefined,
      view,
      view,
      1,
      JSON.stringify(second.checkpoint),
    ),
    /another wallet/,
  );
  const continued = JSON.parse(
    await scanBalanceBatch(
      daemon,
      undefined,
      undefined,
      spend,
      view,
      2,
      JSON.stringify(second.checkpoint),
    ),
  );
  assert.equal(continued.scannedHeight, 301);
  const changedAnchor = {
    ...second.checkpoint,
    spans: second.checkpoint.spans.map((span) => ({ ...span, anchorHash: '00'.repeat(32) })),
  };
  const reset = JSON.parse(
    await scanBalanceBatch(
      daemon,
      undefined,
      undefined,
      spend,
      view,
      1,
      JSON.stringify(changedAnchor),
    ),
  );
  assert.equal(reset.scannedHeight, 101, 'changed chain anchor must discard old scan state');
});

test('tip checkpoint refresh returns zero for test keys and cancellation rejects before network', async () => {
  const { height } = await (await fetch(`${daemon}/get_height`)).json();
  const result = JSON.parse(
    await scanBalanceBatch(daemon, undefined, undefined, spend, view, height - 5),
  );
  assert.equal(result.done, true);
  assert.equal(result.balanceAtomic, '0');
  assert.equal(result.lockedAtomic, '0');
  const refreshed = JSON.parse(
    await scanBalanceBatch(
      daemon,
      undefined,
      undefined,
      spend,
      view,
      height - 5,
      JSON.stringify(result.checkpoint),
    ),
  );
  assert.equal(refreshed.scannedHeight, result.scannedHeight);
  assert.equal(refreshed.done, true);
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    scanBalanceBatch(daemon, undefined, undefined, spend, view, 1, undefined, {
      signal: controller.signal,
    }),
    /aborted/,
  );
});
