import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { setImmediate as nextTurn } from 'node:timers/promises';

const require = createRequire(import.meta.url);
const wasm = require('../pkg-node/zebgen_monero_wasm.js');
const originalSetTimeout = globalThis.setTimeout;
const originalClearTimeout = globalThis.clearTimeout;
const timers = new Set();
const responseLimit = 65_535 + 8 * 1_024;
let deadlineMs = 2_000;
let expired = 0;
let started = 0;

globalThis.setTimeout = (callback, delay, ...args) => {
  if (delay !== 60_000) return originalSetTimeout(callback, delay, ...args);
  started += 1;
  const handle = originalSetTimeout(() => {
    timers.delete(handle);
    expired += 1;
    callback(...args);
  }, deadlineMs);
  timers.add(handle);
  return handle;
};
globalThis.clearTimeout = (handle) => {
  timers.delete(handle);
  originalClearTimeout(handle);
};

async function scenario(name, handler, check) {
  const failures = [];
  const server = createServer((request, response) => {
    request.resume();
    Promise.resolve(handler(request, response)).catch((error) => {
      failures.push(error);
      response.destroy(error);
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const url = `http://127.0.0.1:${server.address().port}`;
  try {
    await check(url);
    assert.equal(timers.size, 0, 'transport timer was cleared');
    assert.deepEqual(failures, []);
    console.log(`ok - ${name}`);
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
}

function success(request, response) {
  response.end(request.url === '/get_height' ? '{"height":42}' : '[]');
}

try {
  await scenario('valid responses and exact byte boundary', (request, response) => {
    const json = request.url === '/get_height' ? '{"height":42}' : '[]';
    response.end(json.padEnd(responseLimit, ' '));
  }, async (url) => {
    assert.equal(await wasm.daemonHeight(url), 42);
  });

  for (const authenticated of [false, true]) {
    let closed;
    let requests = 0;
    await scenario(`early cumulative limit, ${authenticated ? '401' : '200'} body`, async (request, response) => {
      requests += 1;
      closed = new Promise((resolve) => response.once('close', resolve));
      if (authenticated) {
        response.writeHead(401, { 'WWW-Authenticate': 'Digest realm="test", nonce="nonce", qop="auth"' });
      }
      response.write(Buffer.alloc(40_000, 32));
      await nextTurn();
      response.write(Buffer.alloc(40_000, 32));
    }, async (url) => {
      await assert.rejects(wasm.daemonHeight(url, 'user', 'password'), /exceeded size limit/);
      await closed;
      assert.equal(requests, 1, 'oversized challenge must not be retried');
    });
  }

  let challenges = 0;
  let authorized = 0;
  const beforeDigest = started;
  await scenario('normal Digest challenge drains and cached authorization succeeds', (request, response) => {
    if (!request.headers.authorization) {
      challenges += 1;
      response.writeHead(401, { 'WWW-Authenticate': 'Digest realm="test", nonce="nonce", qop="auth"' });
      response.end('normal challenge body');
      return;
    }
    assert.match(request.headers.authorization, /^Digest /);
    authorized += 1;
    success(request, response);
  }, async (url) => {
    assert.equal(await wasm.daemonHeight(url, 'user', 'password'), 42);
    assert.equal(challenges, 1);
    assert.equal(authorized, 2);
    assert.equal(started, beforeDigest + 2, 'auth retry shares the original deadline');
  });

  deadlineMs = 100;
  let activeRequests = 0;
  let peakRequests = 0;
  await scenario('authenticated transports serialize concurrent callers through body completion', async (request, response) => {
    activeRequests += 1;
    peakRequests = Math.max(peakRequests, activeRequests);
    response.once('finish', () => { activeRequests -= 1; });
    response.write(' ');
    await nextTurn();
    success(request, response);
  }, async (url) => {
    const heights = await Promise.all(Array.from({ length: 8 }, () => wasm.daemonHeight(url, 'user', 'password')));
    assert.deepEqual(heights, Array(8).fill(42));
    assert.equal(peakRequests, 1);
  });

  for (const stage of ['headers', 'body', 'challenge', 'retry body']) {
    let closed;
    const before = expired;
    await scenario(`deadline aborts stalled ${stage}`, (request, response) => {
      closed = new Promise((resolve) => response.once('close', resolve));
      if (stage === 'headers') return;
      if (stage === 'challenge' || (stage === 'retry body' && !request.headers.authorization)) {
        response.writeHead(401, { 'WWW-Authenticate': 'Digest realm="test", nonce="nonce", qop="auth"' });
        if (stage === 'retry body') {
          response.end('challenge');
          return;
        }
      }
      response.write(' ');
    }, async (url) => {
      await assert.rejects(wasm.daemonHeight(url, 'user', 'password'), /fetch:|reading body:/);
      await closed;
      assert.equal(expired, before + 1);
    });
  }

  await scenario('HTTP errors remain exceptions', (_request, response) => {
    response.writeHead(503);
    response.write('unfinished error body');
  }, async (url) => {
    await assert.rejects(wasm.daemonHeight(url), /HTTP 503/);
  });

  for (const credentials of [false, true]) {
    await scenario(`401 errors clean up ${credentials ? 'rejected login' : 'missing login'}`, (_request, response) => {
      response.writeHead(401, { 'WWW-Authenticate': 'Basic realm="test"' });
      response.end('login required');
    }, async (url) => {
      await assert.rejects(
        credentials ? wasm.daemonHeight(url, 'user', 'password') : wasm.daemonHeight(url),
        credentials ? /login rejected/ : /requires a login/,
      );
    });
  }

  await scenario('network errors remain exceptions', (request) => {
    request.socket.destroy();
  }, async (url) => {
    await assert.rejects(wasm.daemonHeight(url), /fetch:/);
  });

  console.log('PASS: transport limits');
} finally {
  for (const handle of timers) originalClearTimeout(handle);
  globalThis.setTimeout = originalSetTimeout;
  globalThis.clearTimeout = originalClearTimeout;
}