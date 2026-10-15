// Ink: someone watching a screen share draws on it. The strokes are passed on
// to the rest of the room as they are drawn and kept nowhere.
//
//   node --test server/test/ink.test.js

import { after, before, describe, it } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';

const here = dirname(fileURLToPath(import.meta.url));
const serverEntry = resolve(here, '..', 'src', 'index.js');

// Unique across the suite: node --test runs these files in PARALLEL.
const FAKE_MTX_PORT = 20012;
const HARMONY_PORT = 18096;
const BASE = `http://127.0.0.1:${HARMONY_PORT}`;
const WS_URL = `ws://127.0.0.1:${HARMONY_PORT}/ws`;

let fakeMtx;
let child;
let dataDir;
let ownerKey = null;
let ownerToken;
let memberToken;
let thirdToken;
let voiceChannelId;
function startFakeMediaMtx() {
  return new Promise((done) => {
    fakeMtx = http.createServer((_req, res) => {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ itemCount: 0, pageCount: 0, items: [] }));
    });
    fakeMtx.listen(FAKE_MTX_PORT, '127.0.0.1', done);
  });
}

function startHarmony() {
  return new Promise((done, fail) => {
    child = spawn(process.execPath, [serverEntry], {
      env: {
        ...process.env,
        HARMONY_PORT: String(HARMONY_PORT),
        HARMONY_HOST: '127.0.0.1',
        HARMONY_DATA_DIR: dataDir,
        HARMONY_MEDIAMTX_API: `http://127.0.0.1:${FAKE_MTX_PORT}`,
        HARMONY_POLL_INTERVAL_MS: '500',
        HARMONY_SIGNALING_URL: 'http://media.test:8889',
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    const timer = setTimeout(() => fail(new Error('server did not start')), 10_000);
    let out = '';
    child.stdout.on('data', (b) => {
      out += b.toString();
      const match = /^\s*│\s+([A-Za-z0-9_-]{20,})\s+│$/m.exec(out);
      if (match) ownerKey = match[1];
      if (out.includes('control server on')) {
        clearTimeout(timer);
        done();
      }
    });
    child.stderr.on('data', (b) => process.stderr.write(`[server] ${b}`));
  });
}

const api = async (path, { method = 'GET', body, token, raw, contentType } = {}) => {
  const res = await fetch(`${BASE}${path}`, {
    method,
    headers: {
      ...(body ? { 'Content-Type': 'application/json' } : {}),
      ...(raw ? { 'Content-Type': contentType } : {}),
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    ...(body ? { body: JSON.stringify(body) } : {}),
    ...(raw ? { body: raw } : {}),
  });
  let json = null;
  try { json = await res.json(); } catch { /* empty */ }
  return { status: res.status, body: json };
};

function connect(token) {
  const ws = new WebSocket(WS_URL);
  const inbox = [];
  const waiters = [];
  ws.addEventListener('message', (event) => {
    const msg = JSON.parse(event.data);
    inbox.push(msg);
    for (let i = waiters.length - 1; i >= 0; i -= 1) {
      if (waiters[i].match(msg)) {
        waiters[i].resolve(msg);
        waiters.splice(i, 1);
      }
    }
  });

  const client = {
    ws,
    next(match, { timeoutMs = 5000 } = {}) {
      const found = inbox.find(match);
      if (found) return Promise.resolve(found);
      return new Promise((done, fail) => {
        const timer = setTimeout(
          () => fail(new Error(`timed out; saw ${JSON.stringify(inbox.map((m) => m.type))}`)),
          timeoutMs,
        );
        waiters.push({ match, resolve: (m) => { clearTimeout(timer); done(m); } });
      });
    },
    async request(payload) {
      const rid = Math.random().toString(36).slice(2);
      ws.send(JSON.stringify({ ...payload, rid }));
      return client.next((m) => m.rid === rid);
    },
    async hello() {
      if (ws.readyState !== WebSocket.OPEN) {
        await new Promise((done, fail) => {
          ws.addEventListener('open', done, { once: true });
          ws.addEventListener('close', () => fail(new Error('closed')), { once: true });
        });
      }
      return client.request({ type: 'hello', token });
    },
  };
  return client;
}

before(async () => {
  dataDir = mkdtempSync(resolve(tmpdir(), 'harmony-ink-'));
  await startFakeMediaMtx();
  await startHarmony();
  const register = async (nickname, extra = {}) => (await api('/api/accounts/register', {
    method: 'POST', body: { nickname, password: 'hunter22', ...extra },
  })).body.token;
  ownerToken = await register('boss', { ownerKey });
  memberToken = await register('member');
  thirdToken = await register('third');
  voiceChannelId = (await api('/api/channels', { token: ownerToken }))
    .body.channels.find((c) => c.kind === 'voice').id;
});

after(async () => {
  await new Promise((done) => {
    if (!child || child.exitCode !== null) return done();
    child.once('exit', done);
    child.kill('SIGTERM');
    setTimeout(() => { child.kill('SIGKILL'); done(); }, 4000).unref();
  });
  await new Promise((done) => fakeMtx.close(done));
  rmSync(dataDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
});

// ---------------------------------------------------------------------------

/** The owner shares their screen; the member and the third person watch. */
async function room() {
  const sharer = connect(ownerToken);
  const viewer = connect(memberToken);
  const other = connect(thirdToken);
  await sharer.hello(); await viewer.hello(); await other.hello();
  const joined = await sharer.request({ type: 'voice:join', channelId: voiceChannelId });
  await viewer.request({ type: 'voice:join', channelId: voiceChannelId });
  await other.request({ type: 'voice:join', channelId: voiceChannelId });
  await sharer.request({ type: 'voice:publishing', channelId: voiceChannelId, kind: 's', on: true });
  const close = async () => {
    for (const c of [sharer, viewer, other]) {
      await c.request({ type: 'voice:leave', channelId: voiceChannelId });
      c.ws.close();
    }
  };
  return { sharer, viewer, other, mid: joined.mid, close };
}

const stroke = (mid, extra = {}) => ({
  type: 'ink', channelId: voiceChannelId, mid, id: 1, color: '#ff4d6d', points: [100, 200, 300, 400], ...extra,
});

describe('ink on a screen share', () => {
  it('reaches the sharer and the other viewers, not the one drawing', async () => {
    const r = await room();
    const toSharer = r.sharer.next((m) => m.type === 'ink');
    const toOther = r.other.next((m) => m.type === 'ink');
    const ok = await r.viewer.request(stroke(r.mid, { end: true }));
    assert.equal(ok.type, 'voice:ok');
    for (const event of [await toSharer, await toOther]) {
      assert.equal(event.mid, r.mid);
      assert.equal(event.channelId, voiceChannelId);
      assert.deepEqual(event.points, [100, 200, 300, 400]);
      assert.equal(event.color, '#ff4d6d');
      assert.equal(event.end, true);
      assert.equal(event.clear, false);
      assert.equal(typeof event.by, 'number');
    }
    await assert.rejects(r.viewer.next((m) => m.type === 'ink', { timeoutMs: 300 }));
    await r.close();
  });

  it('is refused on someone who is not sharing', async () => {
    const r = await room();
    const viewerMid = (await r.viewer.request({ type: 'voice:join', channelId: voiceChannelId })).mid;
    const res = await r.other.request(stroke(viewerMid ?? r.mid + 1));
    assert.equal(res.error, 'not_sharing');
    await r.close();
  });

  it('is refused from somebody not in the channel', async () => {
    const r = await room();
    await r.other.request({ type: 'voice:leave', channelId: voiceChannelId });
    const res = await r.other.request(stroke(r.mid));
    assert.equal(res.error, 'not_in_channel');
    await r.close();
  });

  it('refuses points off the picture, an odd count, or a colour that is not one', async () => {
    const r = await room();
    for (const bad of [{ points: [0, 10_001] }, { points: [1, 2, 3] }, { points: [1.5, 2] }, { color: 'red' }, { id: -1 }]) {
      const res = await r.viewer.request(stroke(r.mid, bad));
      assert.equal(res.error, 'bad_ink', JSON.stringify(bad));
    }
    await r.close();
  });

  it('is advertised in health', async () => {
    const res = await api('/api/health');
    assert.ok(res.body.features.includes('ink'));
  });
});
