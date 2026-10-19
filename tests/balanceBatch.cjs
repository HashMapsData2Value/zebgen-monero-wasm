const assert = require('node:assert/strict');
const { test } = require('node:test');
const { scanBalanceBatch } = require('../pkg-node/zebgen_monero_wasm.js');

const daemon = 'http://127.0.0.1:18081';
const spend = '01' + '00'.repeat(31);
const view = '02' + '00'.repeat(31);

test('bounded scan resumes its serialized cursor and rejects a different wallet or restore height', async () => {
  const first = JSON.parse(await scanBalanceBatch(daemon, undefined, undefined, spend, view, 1));
  assert.equal(first.scannedHeight, 101);
  assert.equal(first.done, false);
  assert.equal(first.checkpoint.outputs.length, 0);
  const second = JSON.parse(await scanBalanceBatch(daemon, undefined, undefined, spend, view, 1, JSON.stringify(first.checkpoint)));
  assert.equal(second.scannedHeight, 201);
  assert.equal(second.checkpoint.restoreHeight, 1);
  await assert.rejects(scanBalanceBatch(daemon, undefined, undefined, view, view, 1, JSON.stringify(second.checkpoint)), /another wallet/);
  await assert.rejects(scanBalanceBatch(daemon, undefined, undefined, spend, view, 2, JSON.stringify(second.checkpoint)), /restore height/);
  const changedAnchor = { ...second.checkpoint, anchorHash: '00'.repeat(32) };
  const reset = JSON.parse(await scanBalanceBatch(daemon, undefined, undefined, spend, view, 1, JSON.stringify(changedAnchor)));
  assert.equal(reset.scannedHeight, 101, 'changed chain anchor must discard old scan state');
});

test('tip checkpoint refresh returns zero for test keys and cancellation rejects before network', async () => {
  const { height } = await (await fetch(`${daemon}/get_height`)).json();
  const result = JSON.parse(await scanBalanceBatch(daemon, undefined, undefined, spend, view, height - 5));
  assert.equal(result.done, true);
  assert.equal(result.balanceAtomic, '0');
  const refreshed = JSON.parse(await scanBalanceBatch(daemon, undefined, undefined, spend, view, height - 5, JSON.stringify(result.checkpoint)));
  assert.equal(refreshed.scannedHeight, result.scannedHeight);
  assert.equal(refreshed.done, true);
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(scanBalanceBatch(daemon, undefined, undefined, spend, view, 1, undefined, { signal: controller.signal }), /aborted/);
});