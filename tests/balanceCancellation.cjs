const assert = require('node:assert/strict');
const { test } = require('node:test');
const { unlockedBalance } = require('../pkg-node/zebgen_monero_wasm.js');

const daemon = process.env.MONERO_TEST_DAEMON ?? 'http://127.0.0.1:18081';
const spend = '01' + '00'.repeat(31);
const view = '02' + '00'.repeat(31);

test('balance scan stops between batches and restarts at a higher height', async () => {
  const controller = new AbortController();
  let updates = 0;
  await assert.rejects(
    unlockedBalance(daemon, undefined, undefined, spend, view, 0, {
      signal: controller.signal,
      onProgress(progress) {
        updates++;
        assert.equal(progress.scannedHeight, 100);
        assert.ok(progress.targetHeight > 100);
        controller.abort();
      },
    }),
    /aborted/,
  );
  assert.equal(updates, 1);
  const response = await fetch(daemon + '/get_height');
  const { height } = await response.json();
  assert.equal(await unlockedBalance(daemon, undefined, undefined, spend, view, height - 5), '0');
});

test('balance scan aborts an in-flight fetch without waiting for an RPC timeout', async () => {
  const originalFetch = global.fetch;
  const controller = new AbortController();
  let requestAborted = false;
  global.fetch = (request) =>
    new Promise((resolve, reject) => {
      request.signal.addEventListener(
        'abort',
        () => {
          requestAborted = true;
          reject(new DOMException('Aborted', 'AbortError'));
        },
        { once: true },
      );
      controller.abort();
    });
  try {
    await assert.rejects(
      unlockedBalance(daemon, undefined, undefined, spend, view, 0, { signal: controller.signal }),
    );
    assert.equal(requestAborted, true);
  } finally {
    global.fetch = originalFetch;
  }
});
