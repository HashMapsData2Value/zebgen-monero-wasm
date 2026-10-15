import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const wasm = require('../target/transport-tests/zebgen_monero_wasm.js');
const privateKey = '01' + '00'.repeat(31);
const publicKey = wasm.publicKeyFromPrivate(privateKey);
const daemon = 'http://127.0.0.1:18081';

function trackedController() {
  const controller = new AbortController();
  const signal = controller.signal;
  const listeners = new Set();
  const add = signal.addEventListener.bind(signal);
  const remove = signal.removeEventListener.bind(signal);
  signal.addEventListener = (type, listener, options) => {
    if (type === 'abort') listeners.add(listener);
    return add(type, listener, options);
  };
  signal.removeEventListener = (type, listener, options) => {
    if (type === 'abort') listeners.delete(listener);
    return remove(type, listener, options);
  };
  return { controller, signal, listeners };
}

async function scenario(name, handler, check) {
  const failures = [];
  const server = createServer((request, response) => {
    Promise.resolve(handler(request, response)).catch((error) => {
      failures.push(error);
      response.destroy(error);
    });
  });
  server.requestTimeout = 2_000;
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  try {
    await check(`http://127.0.0.1:${server.address().port}`);
    assert.deepEqual(failures, []);
    console.log(`ok - ${name}`);
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
}

let requests = 0;
await scenario('invalid controls and pre-aborted signal reject before network', (request, response) => {
  requests += 1;
  request.resume();
  response.end('{}');
}, async (url) => {
  const tracked = trackedController();
  tracked.controller.abort();
  await assert.rejects(
    wasm.scanFunding(url, undefined, undefined, publicKey, privateKey, 0, { signal: tracked.signal }),
    /funding scan aborted/,
  );
  assert.equal(tracked.listeners.size, 0);
  for (const options of [false, 1, 'bad', [], () => {}, { signal: null }, { signal: {} },
    { onProgress: null }, { onProgress: 1 }]) {
    await assert.rejects(wasm.scanFunding(url, undefined, undefined, publicKey, privateKey, 0, options),
      /options must be an object|signal must be an AbortSignal|onProgress must be a function/);
  }
  await assert.rejects(wasm.scanFunding(url, undefined, undefined, publicKey, privateKey, 0, {
    get signal() { throw new Error('getter failure'); },
  }), /unable to read scanFunding signal/);
  await assert.rejects(wasm.scanFunding(url, undefined, undefined, publicKey, privateKey, 0, {
    get onProgress() { throw new Error('getter failure'); },
  }), /unable to read scanFunding onProgress/);
  assert.equal(requests, 0);
});

for (const stage of ['headers', 'body']) {
  const tracked = trackedController();
  let closed;
  let count = 0;
  await scenario(`signal event aborts stalled ${stage} and removes listener`, (request, response) => {
    count += 1;
    request.resume();
    closed = new Promise((resolve) => response.once('close', resolve));
    if (stage === 'body') response.write(' ');
    setImmediate(() => tracked.controller.abort());
  }, async (url) => {
    await assert.rejects(wasm.scanFunding(url, undefined, undefined, publicKey, privateKey, 0,
      { signal: tracked.signal }), /fetch:|reading body:/);
    await closed;
    assert.equal(count, 1);
    assert.equal(tracked.listeners.size, 0);
  });
}

await scenario('six args, empty options and no-batch scan retain result shape', (request, response) => {
  request.resume();
  response.end(request.url === '/get_height' ? '{"height":42}' : '[]');
}, async (url) => {
  const expected = {
    receivedAtomic: '0', matureAtomic: '0', lockedAtomic: '0',
    scannedHeight: 42, spentStatusKnown: false,
  };
  assert.deepEqual(await wasm.scanFunding(url, undefined, undefined, publicKey, privateKey, 42), expected);
  for (const options of [undefined, null, {}]) {
    assert.deepEqual(await wasm.scanFunding(url, undefined, undefined, publicKey, privateKey, 42, options), expected);
  }
  const tracked = trackedController();
  let callbacks = 0;
  assert.deepEqual(await wasm.scanFunding(url, undefined, undefined, publicKey, privateKey, 42, {
    signal: tracked.signal, onProgress() { callbacks += 1; },
  }), expected);
  assert.equal(callbacks, 0);
  assert.equal(tracked.listeners.size, 0);
});

const info = await fetch(`${daemon}/json_rpc`, {
  method: 'POST',
  body: JSON.stringify({ jsonrpc: '2.0', id: 0, method: 'get_info' }),
  signal: AbortSignal.timeout(2_000),
}).then((response) => response.json());
assert.equal(info.result?.nettype, 'fakechain', 'funding tests require the isolated local regtest daemon');
const height = await wasm.daemonHeight(daemon);
assert.ok(height >= 201, 'regtest needs at least 201 existing blocks; this test does not mine');
const restore = height - 201;
const scanArgs = [daemon, undefined, undefined, publicKey, privateKey, restore];
const baseline = await wasm.scanFunding(...scanArgs);
const tracked = trackedController();
const progress = [];
const result = await wasm.scanFunding(...scanArgs, {
  signal: tracked.signal,
  onProgress(value) { progress.push(value); },
});
assert.deepEqual(result, baseline);
assert.deepEqual(progress, [
  { scannedHeight: restore + 100, targetHeight: height },
  { scannedHeight: restore + 200, targetHeight: height },
  { scannedHeight: height, targetHeight: height },
]);
assert.equal(tracked.listeners.size, 0);
console.log('ok - exactly one public height-only progress object per completed batch');

async function forward(request, response) {
  const chunks = [];
  for await (const chunk of request) chunks.push(chunk);
  const upstream = await fetch(`${daemon}${request.url}`, {
    method: 'POST', body: Buffer.concat(chunks), signal: AbortSignal.timeout(2_000),
  });
  response.writeHead(upstream.status);
  response.end(Buffer.from(await upstream.arrayBuffer()));
}

{
  const tracked = trackedController();
  let closed;
  let batches = 0;
  await scenario('abort cancels stalled batch body after connection succeeds', (request, response) => {
    if (!request.url.includes('/get_blocks')) return forward(request, response);
    batches += 1;
    request.resume();
    closed = new Promise((resolve) => response.once('close', resolve));
    response.write(' ');
    setImmediate(() => tracked.controller.abort());
  }, async (url) => {
    await assert.rejects(wasm.scanFunding(url, undefined, undefined, publicKey, privateKey, restore,
      { signal: tracked.signal }), /fetch:|reading body:/);
    await closed;
    assert.equal(batches, 1);
    assert.equal(tracked.listeners.size, 0);
  });
}

for (const action of ['abort', 'throw']) {
  const tracked = trackedController();
  let callbacks = 0;
  let requestsAfterCallback = 0;
  await scenario(`callback ${action} stops scan without affecting concurrent scan`, (request, response) => {
    if (callbacks) requestsAfterCallback += 1;
    return forward(request, response);
  }, async (url) => {
    const scan = wasm.scanFunding(url, undefined, undefined, publicKey, privateKey, restore, {
      signal: tracked.signal,
      onProgress(value) {
        callbacks += 1;
        assert.deepEqual(value, { scannedHeight: restore + 100, targetHeight: height });
        if (action === 'throw') throw new Error('progress failed');
        tracked.controller.abort();
      },
    });
    const independent = wasm.scanFunding(...scanArgs);
    await assert.rejects(scan, action === 'abort' ? /funding scan aborted/ : /onProgress callback threw.*progress failed/);
    assert.deepEqual(await independent, baseline, 'other scans are unaffected');
    assert.equal(callbacks, 1, 'no subsequent batch callback after cancellation');
    assert.equal(requestsAfterCallback, 0, 'no subsequent batch request after cancellation');
    assert.equal(tracked.listeners.size, 0);
  });
}

console.log('PASS: funding controls (local HTTP and read-only regtest)');