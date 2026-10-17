// The operations API: /ops/*, the host's admin panel's way in.
//
// Its own file, and its own server, because several of these tests change
// things every other file takes for granted -- the door password, who owns
// the server -- and a second, token-less server to prove the API is not
// there at all without one.
//
//   node --test server/test/ops.test.js

import { after, before, describe, it } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';

const here = dirname(fileURLToPath(import.meta.url));
const serverEntry = resolve(here, '..', 'src', 'index.js');

// Unique across the suite: node --test runs these files in PARALLEL.
const FAKE_MTX_PORT = 20015;
const HARMONY_PORT = 18099;
const BARE_PORT = 18100;
const TOKEN = 'ops-token-for-the-tests-0123456789';

let fakeMtx;
let child;
let dataDir;
let ownerKey;
let ownerToken;
let ownerId;

function startFakeMediaMtx() {
  return new Promise((done) => {
    fakeMtx = http.createServer((_req, res) => {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ itemCount: 0, pageCount: 0, items: [] }));
    });
    fakeMtx.listen(FAKE_MTX_PORT, '127.0.0.1', done);
  });
}

function startHarmony({ port, dir, env = {} }) {
  return new Promise((done, fail) => {
    const proc = spawn(process.execPath, [serverEntry], {
      env: {
        ...process.env,
        HARMONY_OPS_TOKEN: '',
        HARMONY_BUILD: '',
        HARMONY_PORT: String(port),
        HARMONY_HOST: '127.0.0.1',
        HARMONY_DATA_DIR: dir,
        HARMONY_MEDIAMTX_API: `http://127.0.0.1:${FAKE_MTX_PORT}`,
        HARMONY_POLL_INTERVAL_MS: '500',
        HARMONY_SIGNALING_URL: 'http://media.test:8889',
        ...env,
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    const timer = setTimeout(() => fail(new Error('server did not start')), 10_000);
    let out = '';
    proc.stdout.on('data', (buf) => {
      out += buf.toString();
      if (out.includes('control server on')) {
        clearTimeout(timer);
        done({ proc, log: () => out });
      }
    });
    proc.stderr.on('data', (b) => {
      out += b.toString();
      process.stderr.write(`[server] ${b}`);
    });
  });
}

function stop(proc) {
  return new Promise((done) => {
    if (!proc || proc.exitCode !== null) return done();
    proc.once('exit', () => done());
    proc.kill('SIGTERM');
    setTimeout(() => { proc.kill('SIGKILL'); done(); }, 4000).unref();
  });
}

const request = async (port, path, { method = 'GET', body, headers = {} } = {}) => {
  const res = await fetch(`http://127.0.0.1:${port}${path}`, {
    method,
    headers: { ...(body ? { 'Content-Type': 'application/json' } : {}), ...headers },
    ...(body ? { body: JSON.stringify(body) } : {}),
  });
  const text = await res.text();
  let json = null;
  try { json = JSON.parse(text); } catch { /* not JSON */ }
  return { status: res.status, body: json, text, headers: res.headers };
};

/** The panel's call: the right token, straight from this machine. */
const ops = (path, opts = {}) => request(HARMONY_PORT, `/ops${path}`, {
  ...opts, headers: { Authorization: `Bearer ${TOKEN}`, ...opts.headers },
});

/** A member's call. `password` is the SHARED door key, not an account's. */
const api = (path, { token, password, ...opts } = {}) => request(HARMONY_PORT, path, {
  ...opts,
  headers: {
    ...(token ? { Authorization: `Bearer ${token}` } : {}),
    ...(password ? { 'x-harmony-password': password } : {}),
  },
});

const register = async (nickname, password = 'hunter22', extra = {}) => {
  const res = await api('/api/accounts/register', { method: 'POST', body: { nickname, password, ...extra } });
  assert.equal(res.status, 201, JSON.stringify(res.body));
  return { id: res.body.user.id, token: res.body.token };
};

const login = (nickname, password = 'hunter22', extra = {}) => api('/api/accounts/login', {
  method: 'POST', body: { nickname, password, ...extra },
});

/** A signed-in socket, and the code it is eventually closed with. */
async function connect(token) {
  const ws = new WebSocket(`ws://127.0.0.1:${HARMONY_PORT}/ws`);
  const closed = new Promise((done) => ws.addEventListener('close', (e) => done(e.code), { once: true }));
  const hello = new Promise((done) => ws.addEventListener('message', (e) => {
    const msg = JSON.parse(e.data);
    if (msg.rid === 'hello') done(msg);
  }));
  await new Promise((done) => ws.addEventListener('open', done, { once: true }));
  ws.send(JSON.stringify({ type: 'hello', token, rid: 'hello' }));
  assert.equal((await hello).type, 'hello-ok');
  return { closed, close: () => ws.close() };
}

const waitFor = async (check) => {
  for (let i = 0; i < 100; i += 1) {
    if (await check()) return;
    await new Promise((done) => setTimeout(done, 50));
  }
  throw new Error('timed out');
};

before(async () => {
  dataDir = mkdtempSync(resolve(tmpdir(), 'harmony-ops-'));
  await startFakeMediaMtx();
  ({ proc: child } = await startHarmony({
    port: HARMONY_PORT, dir: dataDir, env: { HARMONY_OPS_TOKEN: TOKEN, HARMONY_BUILD: 'abc1234' },
  }));

  const key = await ops('/owner-key');
  assert.equal(key.status, 200);
  assert.equal(key.body.claimed, false);
  ownerKey = key.body.key;
  assert.match(ownerKey, /^[A-Za-z0-9_-]{20,}$/);
  ({ id: ownerId, token: ownerToken } = await register('chief', 'hunter22', { ownerKey }));
});

after(async () => {
  await stop(child);
  await new Promise((done) => fakeMtx.close(done));
  rmSync(dataDir, { recursive: true, force: true });
});

describe('the gate', () => {
  it('is not there at all without a token, or with a short one', async () => {
    for (const token of ['', 'too-short']) {
      const dir = mkdtempSync(resolve(tmpdir(), 'harmony-ops-bare-'));
      const { proc, log } = await startHarmony({ port: BARE_PORT, dir, env: { HARMONY_OPS_TOKEN: token } });
      try {
        for (const offered of [token || 'anything', TOKEN]) {
          const res = await request(BARE_PORT, '/ops/status', { headers: { Authorization: `Bearer ${offered}` } });
          assert.equal(res.status, 404);
        }
        assert.equal(log().includes('shorter than 24'), token !== '');
      } finally {
        await stop(proc);
        rmSync(dir, { recursive: true, force: true });
      }
    }
  });

  it('answers a refusal exactly as it answers a path that does not exist', async () => {
    const unknown = await request(HARMONY_PORT, '/ops/no-such-thing');
    const refusals = [
      await request(HARMONY_PORT, '/ops/status'),
      await request(HARMONY_PORT, '/ops/status', { headers: { Authorization: 'Bearer wrong-token-wrong-token-wrong' } }),
      await request(HARMONY_PORT, '/ops/status', { headers: { Authorization: TOKEN } }),
      await ops('/status', { headers: { 'X-Forwarded-For': '127.0.0.1' } }),
      await ops('/status', { headers: { Forwarded: 'for=127.0.0.1' } }),
    ];
    for (const res of refusals) {
      assert.equal(res.status, 404);
      assert.equal(res.text, unknown.text.replace('/ops/no-such-thing', '/ops/status'));
      assert.equal(res.headers.get('access-control-allow-origin'), unknown.headers.get('access-control-allow-origin'));
    }
  });

  it('needs no door password, and its answers carry no CORS invitation', async () => {
    const res = await ops('/status');
    assert.equal(res.status, 200);
    assert.equal(res.headers.get('access-control-allow-origin'), null);
  });
});

describe('status and accounts', () => {
  it('says what the server is and who is on it', async () => {
    const socket = await connect(ownerToken);
    try {
      const { status, body } = await ops('/status');
      assert.equal(status, 200);
      assert.equal(body.name, 'Harmony');
      assert.match(body.version, /^\d+\.\d+\.\d+/);
      assert.equal(body.build, 'abc1234');
      assert.ok(body.startedAt <= Date.now());
      assert.ok(Number.isInteger(body.uptimeSec));
      assert.ok(Number.isInteger(body.schema) && body.schema >= 16);
      assert.equal(body.ownerClaimed, true);
      assert.ok(body.accounts >= 1);
      assert.equal(typeof body.banned, 'number');
      assert.deepEqual(body.online, [{ id: ownerId, nickname: 'chief' }]);
      assert.deepEqual(body.voice, []);
      assert.equal(body.calls, 0);
      assert.deepEqual(Object.keys(body.storage).sort(), ['diskFreeBytes', 'maxFileBytes', 'quotaBytes', 'usedBytes']);
      assert.ok(body.storage.quotaBytes > 0 && body.storage.maxFileBytes > 0);
      assert.ok(body.dbBytes > 0);
    } finally {
      socket.close();
    }
  });

  it('lists every account with whether it is on', async () => {
    const { status, body } = await ops('/accounts');
    assert.equal(status, 200);
    const chief = body.find((u) => u.nickname === 'chief');
    assert.deepEqual(Object.keys(chief).sort(),
      ['bannedAt', 'banned', 'createdAt', 'displayName', 'id', 'nickname', 'online', 'role'].sort());
    assert.equal(chief.role, 'owner');
    assert.equal(chief.banned, false);
    assert.equal(chief.bannedAt, null);
  });
});

describe('managing accounts', () => {
  it('kicks somebody out everywhere, and lets them back in', async () => {
    const { id, token } = await register('kickme');
    const socket = await connect(token);
    const res = await ops(`/accounts/${id}/kick`, { method: 'POST' });
    assert.equal(res.status, 200);
    assert.equal(await socket.closed, 4401);
    assert.equal((await api('/api/accounts/me', { token })).status, 401);
    assert.equal((await login('kickme')).status, 200);
  });

  it('sets a new password and ends every session', async () => {
    const { id, token } = await register('forgetful');
    const socket = await connect(token);
    assert.equal((await ops(`/accounts/${id}/password`, { method: 'POST', body: { password: '123' } })).status, 400);
    const res = await ops(`/accounts/${id}/password`, { method: 'POST', body: { password: 'brand-new-one' } });
    assert.equal(res.status, 200);
    assert.equal(await socket.closed, 4401);
    assert.equal((await api('/api/accounts/me', { token })).status, 401);
    assert.equal((await login('forgetful')).status, 401);
    assert.equal((await login('forgetful', 'brand-new-one')).status, 200);
  });

  it('bans: no sign-in, no session, no new account under the name, no place in the list', async () => {
    const { id, token } = await register('troll');
    const socket = await connect(token);
    const res = await ops(`/accounts/${id}/ban`, { method: 'POST' });
    assert.equal(res.status, 200);
    assert.equal(res.body.user.banned, true);
    assert.ok(res.body.user.bannedAt > 0);
    assert.equal(await socket.closed, 4401);

    assert.equal((await api('/api/accounts/me', { token })).status, 401);
    const refused = await login('troll');
    assert.equal(refused.status, 403);
    assert.equal(refused.body.error, 'banned');
    // The wrong password still says only that, so a ban cannot be probed for.
    assert.equal((await login('troll', 'not-it')).body.error, 'bad_credentials');
    const again = await api('/api/accounts/register', { method: 'POST', body: { nickname: 'troll', password: 'hunter22' } });
    assert.equal(again.body.error, 'nickname_taken');

    const members = (await api('/api/accounts', { token: ownerToken })).body.users;
    assert.ok(!members.some((u) => u.nickname === 'troll'));
    assert.ok((await ops('/accounts')).body.find((u) => u.nickname === 'troll').banned);
    assert.equal((await ops('/status')).body.banned, 1);

    const back = await ops(`/accounts/${id}/unban`, { method: 'POST' });
    assert.equal(back.status, 200);
    assert.equal(back.body.user.banned, false);
    assert.equal((await login('troll')).status, 200);
    assert.ok((await api('/api/accounts', { token: ownerToken })).body.users.some((u) => u.nickname === 'troll'));
  });

  it('will not ban the owner', async () => {
    const res = await ops(`/accounts/${ownerId}/ban`, { method: 'POST' });
    assert.equal(res.status, 403);
    assert.equal(res.body.error, 'cannot_ban_owner');
    assert.equal((await ops('/accounts/99999/ban', { method: 'POST' })).status, 404);
  });

  it('makes and unmakes admins, and leaves the owner alone', async () => {
    const { id } = await register('helper');
    const up = await ops(`/accounts/${id}/role`, { method: 'POST', body: { role: 'admin' } });
    assert.equal(up.status, 200);
    assert.equal(up.body.user.role, 'admin');
    const down = await ops(`/accounts/${id}/role`, { method: 'POST', body: { role: 'member' } });
    assert.equal(down.body.user.role, 'member');
    assert.equal((await ops(`/accounts/${id}/role`, { method: 'POST', body: { role: 'owner' } })).status, 400);
    const owner = await ops(`/accounts/${ownerId}/role`, { method: 'POST', body: { role: 'member' } });
    assert.equal(owner.status, 403);
    assert.equal(owner.body.error, 'cannot_change_owner');
  });

  it('deletes an account, but never the owner', async () => {
    const { id } = await register('leaving');
    const res = await ops(`/accounts/${id}/delete`, { method: 'POST' });
    assert.equal(res.status, 200);
    assert.ok(!(await ops('/accounts')).body.some((u) => u.id === id));
    assert.equal((await ops(`/accounts/${ownerId}/delete`, { method: 'POST' })).status, 403);
  });
});

describe('the server', () => {
  it('is renamed', async () => {
    const res = await ops('/server', { method: 'POST', body: { name: 'Lab one' } });
    assert.equal(res.status, 200);
    assert.equal(res.body.server.name, 'Lab one');
    assert.equal((await api('/api/health')).body.name, 'Lab one');
    assert.equal((await ops('/status')).body.name, 'Lab one');
  });

  it('has its door password changed live, sessions kept', async () => {
    const res = await ops('/server-password', { method: 'POST', body: { password: 'front-door-key' } });
    assert.equal(res.status, 200);
    assert.equal(res.body.passwordRequired, true);

    assert.equal((await api('/api/health')).body.passwordRequired, true);
    assert.equal((await api('/api/accounts/me', { token: ownerToken })).status, 401);
    const inside = await api('/api/accounts/me', { token: ownerToken, password: 'front-door-key' });
    assert.equal(inside.status, 200);
    // On the server's own say, not a restart's.
    assert.equal((await api('/api/server', { token: ownerToken, password: 'front-door-key' })).body.server.restartRequired, false);
    assert.equal((await ops('/status')).status, 200);

    const open = await ops('/server-password', { method: 'POST', body: { password: null } });
    assert.equal(open.body.passwordRequired, false);
    assert.equal((await api('/api/accounts/me', { token: ownerToken })).status, 200);
    assert.equal((await ops('/server-password', { method: 'POST', body: { password: 5 } })).status, 400);
  });

  it('streams a backup and keeps no copy', async () => {
    const res = await fetch(`http://127.0.0.1:${HARMONY_PORT}/ops/backup`, { headers: { Authorization: `Bearer ${TOKEN}` } });
    assert.equal(res.status, 200);
    assert.equal(res.headers.get('content-type'), 'application/octet-stream');
    const bytes = Buffer.from(await res.arrayBuffer());
    assert.equal(bytes.subarray(0, 16).toString('latin1'), 'SQLite format 3\0');
    await waitFor(() => !readdirSync(dataDir).some((n) => n.startsWith('.ops-backup-')));
  });
});

// Last: it takes the server away from its owner.
describe('the owner key', () => {
  it('is shown only while the server is unclaimed', async () => {
    assert.deepEqual((await ops('/owner-key')).body, { claimed: true, key: null });
  });

  it('is reset only when asked in so many words, and the owner stays an admin', async () => {
    assert.equal((await ops('/owner-key/reset', { method: 'POST', body: {} })).body.error, 'confirm_required');
    assert.equal((await ops('/owner-key')).body.claimed, true);

    const res = await ops('/owner-key/reset', { method: 'POST', body: { confirm: true } });
    assert.equal(res.status, 200);
    assert.notEqual(res.body.key, ownerKey);
    assert.deepEqual((await ops('/owner-key')).body, { claimed: false, key: res.body.key });
    assert.equal((await ops('/accounts')).body.find((u) => u.id === ownerId).role, 'admin');
    assert.equal((await api('/api/health')).body.needsOwner, true);

    const claimed = await login('chief', 'hunter22', { ownerKey: res.body.key });
    assert.equal(claimed.body.ownerClaimed, true);
    assert.equal(claimed.body.user.role, 'owner');
    assert.deepEqual((await ops('/owner-key')).body, { claimed: true, key: null });
  });
});
