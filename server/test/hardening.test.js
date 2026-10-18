// What an audit found a stranger, or a member, could do that they should not:
// talk the socket into handing over a user row, outlive their media slot,
// shake off a force-mute by rejoining, see who is on a private call, burn the
// Pi's CPU on scrypt, lock the owner out, or fill the SD card with log lines.
// And the two places a throw used to be taken for something else.
//
//   node --test server/test/hardening.test.js

import { after, before, describe, it } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';

import { channelPath } from '../src/channels.js';
import { MediaMtxMonitor } from '../src/mediamtx-api.js';
import { Calls } from '../src/calls.js';

const here = dirname(fileURLToPath(import.meta.url));
const serverEntry = resolve(here, '..', 'src', 'index.js');

// Unique across the suite: node --test runs these files in PARALLEL.
const FAKE_MTX_PORT = 20017;
const HARMONY_PORT = 18102;
const BASE = `http://127.0.0.1:${HARMONY_PORT}`;
const WS_URL = `ws://127.0.0.1:${HARMONY_PORT}/ws`;
const OPS_TOKEN = 'ops-token-for-the-hardening-tests-0123';

let fakeMtx;
let child;
let dataDir;
let ownerKey = null;
let ownerToken;
let stderr = '';
/** What the fake MediaMTX says is on air. */
let livePaths = [];

function startFakeMediaMtx() {
  return new Promise((done) => {
    fakeMtx = http.createServer((req, res) => {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      const items = req.url.startsWith('/v3/paths/list') ? livePaths : [];
      res.end(JSON.stringify({ itemCount: items.length, pageCount: 1, items }));
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
        HARMONY_POLL_INTERVAL_MS: '200',
        HARMONY_SIGNALING_URL: 'http://media.test:8889',
        HARMONY_OPS_TOKEN: OPS_TOKEN,
        HARMONY_PASSWORD: '',
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
    child.stderr.on('data', (b) => { stderr += b.toString(); });
  });
}

/** `from` is the address the request claims, through the loopback proxy the server trusts. */
const api = async (path, { method = 'GET', body, token, from } = {}) => {
  const res = await fetch(`${BASE}${path}`, {
    method,
    headers: {
      ...(body ? { 'Content-Type': 'application/json' } : {}),
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(from ? { 'X-Forwarded-For': from } : {}),
    },
    ...(body ? { body: JSON.stringify(body) } : {}),
  });
  let json = null;
  try { json = await res.json(); } catch { /* 204 */ }
  return { status: res.status, body: json };
};

const ops = (path, { method = 'GET', body } = {}) => fetch(`${BASE}/ops${path}`, {
  method,
  headers: { Authorization: `Bearer ${OPS_TOKEN}`, ...(body ? { 'Content-Type': 'application/json' } : {}) },
  ...(body ? { body: JSON.stringify(body) } : {}),
});

const hook = async (action, path, token) => (await api('/mediamtx/auth', {
  method: 'POST', body: { action, path, query: token ? `token=${encodeURIComponent(token)}` : '' },
})).status;

/** Same shape as realtime.test.js's client, over Node's global WebSocket. */
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
    inbox,
    closed: new Promise((done) => ws.addEventListener('close', (e) => done(e))),
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
    send(payload) {
      ws.send(typeof payload === 'string' ? payload : JSON.stringify(payload));
    },
    async request(payload) {
      const rid = Math.random().toString(36).slice(2);
      this.send({ ...payload, rid });
      return this.next((m) => m.rid === rid);
    },
    async hello() {
      if (ws.readyState !== WebSocket.OPEN) {
        await new Promise((done, fail) => {
          ws.addEventListener('open', done, { once: true });
          ws.addEventListener('close', () => fail(new Error('closed before open')), { once: true });
        });
      }
      return this.request({ type: 'hello', token });
    },
  };
  return client;
}

const register = async (nickname) => (await api('/api/accounts/register', {
  method: 'POST', body: { nickname, password: 'hunter22' },
})).body;

const newVoiceChannel = async (name, extra = {}) => (await api('/api/channels', {
  method: 'POST', body: { kind: 'voice', name, ...extra }, token: ownerToken,
})).body.channel.id;

before(async () => {
  dataDir = mkdtempSync(resolve(tmpdir(), 'harmony-hard-'));
  await startFakeMediaMtx();
  await startHarmony();
  const owner = await api('/api/accounts/register', {
    method: 'POST', body: { nickname: 'boss', password: 'hunter22', ownerKey },
  });
  assert.equal(owner.body.ownerClaimed, true, 'test setup needs a real owner');
  ownerToken = owner.body.token;
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

describe('the socket', () => {
  it('answers a frame that is not JSON, and carries on', async () => {
    const c = connect(ownerToken);
    await c.hello();
    c.send('{not json');
    assert.equal((await c.next((m) => m.error === 'bad_json')).type, 'error');
    assert.equal((await c.request({ type: 'ping' })).type, 'pong');
    c.ws.close();
  });

  it('knows no message type it did not define, whatever Object.prototype has', async () => {
    const c = connect(ownerToken);
    await c.hello();
    for (const type of ['constructor', '__proto__', 'toString', 'hasOwnProperty', 'valueOf', 'isPrototypeOf']) {
      const reply = await c.request({ type });
      assert.equal(reply.error, 'unknown_type', `"${type}" is not a message`);
      const text = JSON.stringify(reply);
      assert.ok(!/password_hash|token_hash|nickname/.test(text), `"${type}" handed back ${text}`);
    }
    c.ws.close();
  });

  it('hangs up on a frame over the limit', async () => {
    const c = connect(ownerToken);
    await c.hello();
    c.send({ type: 'ping', pad: 'x'.repeat(300 * 1024) });
    assert.equal((await c.closed).code, 1009, 'message too big');
  });
});

describe('a force-mute', () => {
  let channelId;
  let member;
  before(async () => {
    channelId = await newVoiceChannel('Mute Room');
    member = await register('mutee');
  });

  it('outlives leaving and rejoining, and a dropped socket, until it is lifted', async () => {
    const admin = connect(ownerToken);
    await admin.hello();
    let c = connect(member.token);
    await c.hello();
    let joined = await c.request({ type: 'voice:join', channelId });
    assert.equal((await admin.request({
      type: 'admin:force-mute', channelId, mid: joined.mid, muted: true,
    })).type, 'voice:ok');

    // Out of the channel and straight back in.
    await c.request({ type: 'voice:leave', channelId });
    joined = await c.request({ type: 'voice:join', channelId });
    assert.equal(joined.roster.find((m) => m.mid === joined.mid).forceMuted, true, 'still shown muted');
    assert.equal(await hook('publish', channelPath(channelId, joined.mid, 'voice'), joined.token), 401,
      'and still refused the microphone');

    // The socket drops, and the client comes back on a new one.
    c.ws.close();
    await c.closed;
    c = connect(member.token);
    await c.hello();
    joined = await c.request({ type: 'voice:join', channelId });
    assert.equal(joined.roster.find((m) => m.mid === joined.mid).forceMuted, true);
    assert.equal(await hook('publish', channelPath(channelId, joined.mid, 'voice'), joined.token), 401);
    // A camera is not the microphone.
    assert.equal(await hook('publish', channelPath(channelId, joined.mid, 'cam'), joined.token), 204);

    await admin.request({ type: 'admin:force-mute', channelId, mid: joined.mid, muted: false });
    assert.equal(await hook('publish', channelPath(channelId, joined.mid, 'voice'), joined.token), 204, 'lifted');
    await c.request({ type: 'voice:leave', channelId });
    joined = await c.request({ type: 'voice:join', channelId });
    assert.equal(joined.roster.find((m) => m.mid === joined.mid).forceMuted, false, 'and stays lifted');
    c.ws.close();
    admin.ws.close();
  });
});

describe('a media token', () => {
  it('stops opening a slot once somebody else holds it', async () => {
    const channelId = await newVoiceChannel('Reuse Room');
    const leaver = connect((await register('leaver')).token);
    await leaver.hello();
    const old = await leaver.request({ type: 'voice:join', channelId });
    await leaver.request({ type: 'voice:leave', channelId });

    const taker = connect((await register('taker')).token);
    await taker.hello();
    const now = await taker.request({ type: 'voice:join', channelId });
    assert.equal(now.mid, old.mid, 'the lowest free slot: the one just left');

    const path = channelPath(channelId, old.mid, 'voice');
    assert.equal(await hook('publish', path, old.token), 401, 'no publishing into the new holder\'s path');
    assert.equal(await hook('read', path, old.token), 401, 'nor listening to a channel they left');
    assert.equal(await hook('publish', path, now.token), 204, 'the holder still can');
    leaver.ws.close();
    taker.ws.close();
  });

  it('stops working the moment its holder is banned', async () => {
    const channelId = await newVoiceChannel('Ban Room');
    const banned = await register('bannedone');
    const c = connect(banned.token);
    await c.hello();
    const joined = await c.request({ type: 'voice:join', channelId });
    const path = channelPath(channelId, joined.mid, 'voice');
    assert.equal(await hook('read', path, joined.token), 204);

    assert.equal((await ops(`/accounts/${banned.user.id}/ban`, { method: 'POST' })).status, 200);
    assert.equal(await hook('read', path, joined.token), 401);
    assert.equal(await hook('publish', path, joined.token), 401);
  });

  it('keeps working for whoever is in the room when its password changes', async () => {
    const channelId = await newVoiceChannel('Door Room');
    const stayer = connect((await register('stayer')).token);
    await stayer.hello();
    const joined = await stayer.request({ type: 'voice:join', channelId });
    const changed = await api(`/api/channels/${channelId}`, {
      method: 'POST', body: { password: 'new-door' }, token: ownerToken,
    });
    assert.equal(changed.status, 200);
    assert.equal(await hook('read', channelPath(channelId, joined.mid, 'voice'), joined.token), 204,
      'a new password changes who may come in, not who is already there');
    stayer.ws.close();
  });
});

describe('the public stream list', () => {
  it('leaves out voice channels and private calls', async () => {
    livePaths = [
      { name: 'dm.5.1.v', ready: true, readyTime: null, tracks: ['Opus'], readers: [] },
      { name: 'vc-1-1-v', ready: true, readyTime: null, tracks: ['Opus'], readers: [] },
      { name: 'streamer', ready: true, readyTime: null, tracks: ['H264'], readers: [] },
    ];
    const watcher = connect(ownerToken);
    await watcher.hello();
    let listed = [];
    for (let i = 0; i < 50 && !listed.includes('streamer'); i += 1) {
      listed = (await api('/api/streams')).body.streams.map((s) => s.username);
      if (!listed.includes('streamer')) await new Promise((done) => { setTimeout(done, 100); });
    }
    assert.deepEqual(listed, ['streamer']);
    const pushed = await watcher.next((m) => m.type === 'streams' && m.streams.length > 0);
    assert.deepEqual(pushed.streams.map((s) => s.username), ['streamer']);
    livePaths = [];
    watcher.ws.close();
  });
});

describe('signing up and signing in', () => {
  it('are paced per address, whoever the name', async () => {
    let last;
    for (let i = 0; i < 31; i += 1) {
      // A weak password is turned away before any scrypt, so the burst is
      // cheap to spend here; it is spent all the same.
      last = await api('/api/accounts/register', {
        method: 'POST', body: { nickname: `flood${i}`, password: 'x' }, from: '10.0.0.50',
      });
    }
    assert.equal(last.status, 429);
    assert.equal(last.body.error, 'slow_down');
    const login = await api('/api/accounts/login', {
      method: 'POST', body: { nickname: 'anyone', password: 'whatever' }, from: '10.0.0.50',
    });
    assert.equal(login.status, 429, 'signing in shares the allowance');
    const elsewhere = await api('/api/accounts/register', {
      method: 'POST', body: { nickname: 'flood-x', password: 'x' }, from: '10.0.0.51',
    });
    assert.equal(elsewhere.status, 400, 'another address has its own');
  });

  it('lock out the address guessing, not the account', async () => {
    await register('target');
    const login = (password, from) => api('/api/accounts/login', {
      method: 'POST', body: { nickname: 'target', password }, from,
    });
    let last;
    for (let i = 0; i < 3; i += 1) last = await login('wrong-guess', '10.0.1.1');
    assert.equal(last.status, 429);
    assert.equal(last.body.error, 'locked_out');
    assert.equal((await login('hunter22', '10.0.1.1')).status, 429, 'the guesser waits');
    assert.equal((await login('hunter22', '10.0.1.2')).status, 200, 'the owner of the account does not');
  });

  it('still lock the account when many addresses grind it together', async () => {
    await register('ground');
    for (let ip = 1; ip <= 10; ip += 1) {
      for (let i = 0; i < 3; i += 1) {
        await api('/api/accounts/login', {
          method: 'POST', body: { nickname: 'ground', password: 'wrong-guess' }, from: `10.0.2.${ip}`,
        });
      }
    }
    const right = await api('/api/accounts/login', {
      method: 'POST', body: { nickname: 'ground', password: 'hunter22' }, from: '10.0.2.99',
    });
    assert.equal(right.status, 429);
    assert.equal(right.body.error, 'locked_out');
  });

  it('meter the current password when changing it', async () => {
    const changer = await register('changer');
    const change = (current) => api('/api/accounts/password', {
      method: 'POST', body: { current, password: 'hunter33' }, token: changer.token, from: '10.0.3.1',
    });
    assert.equal((await change('guess-1')).status, 401);
    assert.equal((await change('guess-2')).status, 401);
    const third = await change('guess-3');
    assert.equal(third.status, 429);
    assert.equal(third.body.error, 'locked_out');
    assert.equal((await change('hunter22')).status, 429, 'not even the right one, until the wait is over');
  });
});

describe('search', () => {
  it('is paced per person', async () => {
    const searcher = await register('searcher');
    const text = (await api('/api/channels', { token: searcher.token })).body.channels.find((c) => c.kind === 'text');
    const statuses = [];
    for (let i = 0; i < 30; i += 1) {
      statuses.push((await api(`/api/channels/${text.id}/search?q=hello${i}`, { token: searcher.token })).status);
    }
    assert.equal(statuses[0], 200);
    assert.ok(statuses.includes(429), `30 searches at once all answered: ${statuses.join(' ')}`);
    assert.equal((await api(`/api/channels/${text.id}/search?q=hello`, { token: ownerToken })).status, 200,
      'somebody else searches on');
  });
});

describe('the log', () => {
  it('does not take a line for every refused media token', async () => {
    const lines = () => stderr.split('\n').filter((l) => l.includes('[auth]')).length;
    const before = lines();
    for (let i = 0; i < 60; i += 1) await hook('read', `vc-1-${(i % 9) + 1}-v`, 'h2.bogus');
    await new Promise((done) => { setTimeout(done, 200); });
    const written = lines() - before;
    assert.ok(written <= 21, `${written} lines for 60 refusals`);
    assert.ok(written >= 1, 'some are still written');
  });
});

// ---------------------------------------------------------------------------
// On their own

describe('the MediaMTX monitor', () => {
  it('does not take a listener that throws for MediaMTX going down', async () => {
    const api = http.createServer((_req, res) => {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ items: [{ name: 'someone', ready: true }] }));
    });
    await new Promise((done) => api.listen(0, '127.0.0.1', done));
    const monitor = new MediaMtxMonitor({ apiUrl: `http://127.0.0.1:${api.address().port}`, intervalMs: 20 });
    const downs = [];
    let heard = 0;
    monitor.on('down', (err) => downs.push(err));
    monitor.on('paths', () => { throw new Error('a listener with a bug'); });
    monitor.on('paths', () => { heard += 1; });
    const warn = console.warn;
    console.warn = () => {};
    try {
      monitor.start();
      await new Promise((done) => { setTimeout(done, 150); });
    } finally {
      monitor.stop();
      console.warn = warn;
      await new Promise((done) => api.close(done));
    }
    assert.deepEqual(downs, []);
    assert.equal(monitor.reachable, true);
    assert.equal(monitor.paths.size, 1);
    assert.ok(heard > 1, 'the next listener still hears every poll');
  });
});

describe('a call whose line cannot be written', () => {
  it('still ends, for both, from the hang-up or the ring timer', async () => {
    const told = [];
    const calls = new Calls({
      ringMs: 20,
      notify: (userIds, payload) => told.push(payload),
      record: () => { throw new Error('SQLITE_FULL: database or disk is full'); },
    });
    const warn = console.warn;
    console.warn = () => {};
    try {
      calls.start({ conversationId: 1, callerId: 1, calleeId: 2, calleeOnline: true });
      assert.deepEqual(calls.hangUp(1, 1), { ok: true });
      assert.ok(told.some((p) => p.type === 'call:state' && p.call.state === 'ended'));

      calls.start({ conversationId: 2, callerId: 1, calleeId: 2, calleeOnline: true });
      await new Promise((done) => { setTimeout(done, 80); });
      assert.equal(calls.get(2), null, 'rang out');
      assert.ok(told.some((p) => p.type === 'call:state' && p.call.conversationId === 2 && p.call.outcome === 'missed'));
    } finally {
      console.warn = warn;
      calls.close();
    }
  });
});
