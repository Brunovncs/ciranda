// Big files in pieces (upload-sessions.js), the owner's per-file limit, and
// the free-disk floor under every upload.
//
//   node --test server/test/uploads.test.js

import { after, before, describe, it } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { existsSync, mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { createHash, randomBytes } from 'node:crypto';
import { Readable } from 'node:stream';

import { Chat, MAX_UPLOAD_BYTES, MAX_FILE_LIMIT } from '../src/chat.js';
import { UploadSessions, PER_USER } from '../src/upload-sessions.js';
import { Throttle } from '../src/throttle.js';
import { openDatabase } from '../src/db.js';

const here = dirname(fileURLToPath(import.meta.url));
const serverEntry = resolve(here, '..', 'src', 'index.js');

// Unique across the suite: node --test runs these files in PARALLEL.
const FAKE_MTX_PORT = 20014;
const HARMONY_PORT = 18098;
const BASE = `http://127.0.0.1:${HARMONY_PORT}`;

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');
/** A chunk as a request body would bring it, saying its length up front. */
const body = (bytes) => Object.assign(Readable.from([bytes]), { headers: { 'content-length': String(bytes.length) } });

// ---------------------------------------------------------------------------
// The pieces, on a database of their own

/** A Chat on a fresh directory, with the free disk under the test's control. */
function scratchChat({ free = () => null, reserve = 1000, quota = 64 * 1024 * 1024 } = {}) {
  const dir = mkdtempSync(resolve(tmpdir(), 'harmony-uploads-'));
  const db = openDatabase({ dataDir: dir });
  const chat = new Chat(db, { dataDir: dir, maxDiskBytes: quota, diskReserveBytes: reserve, diskFree: free });
  return {
    dir,
    db,
    chat,
    incoming: () => readdirSync(resolve(dir, 'uploads', 'incoming')),
    close: () => {
      db.close();
      rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    },
  };
}

describe('the free-disk floor', () => {
  let s;
  let free;
  before(() => { s = scratchChat({ free: () => free, reserve: 1000 }); });
  after(() => s.close());

  it('refuses a file that would leave less than the reserve free', () => {
    free = 1000 + 9;
    assert.deepEqual(s.chat.store(Buffer.from('ten bytes!'), 'text/plain'), { ok: false, error: 'disk_full' });
    assert.equal(s.chat.usedBytes, 0, 'nothing was kept');
    free = 1000 + 10;
    assert.equal(s.chat.store(Buffer.from('ten bytes!'), 'text/plain').ok, true);
  });

  it('counts what is still arriving, and gives it back when released', () => {
    free = 1000 + 100;
    const first = s.chat.reserve(60);
    assert.equal(first.ok, true);
    assert.deepEqual(s.chat.reserve(60), { ok: false, error: 'disk_full' }, 'room for one of them, not both');
    first.release();
    first.release();
    const again = s.chat.reserve(100);
    assert.equal(again.ok, true, 'released once, however many times it was asked');
    again.release();
  });

  it('turns a streamed upload away before reading any of it', async () => {
    free = 1000 + 10;
    let read = 0;
    const source = new Readable({ read() { read += 1; this.push(null); } });
    source.headers = { 'content-length': '5000' };
    assert.deepEqual(await s.chat.receive(source, 'image/png'), { ok: false, error: 'disk_full' });
    assert.equal(read, 0);
    assert.deepEqual(s.incoming(), []);
  });

  it('lets everything through where the disk cannot be asked', () => {
    free = null;
    assert.equal(s.chat.store(Buffer.from('no statfs here'), 'text/plain').ok, true);
  });

  it('measures a real disk by default', () => {
    const real = scratchChat({ free: undefined, reserve: 0 });
    try {
      assert.equal(real.chat.store(Buffer.from('a real disk'), 'text/plain').ok, true);
    } finally {
      real.close();
    }
  });
});

describe('making room', () => {
  it('keeps a file whose row could not be dropped', () => {
    const s = scratchChat({ quota: 30 });
    try {
      // Counted as nobody's, yet a message still names it: the foreign key
      // refuses the row, and the file has to outlive the refusal with it.
      const kept = s.chat.store(Buffer.from('ten bytes!'), 'text/plain').upload.hash;
      const user = s.db.prepare("INSERT INTO users (nickname, password_hash, created_at) VALUES ('ana', 'x', 0) RETURNING id").get().id;
      const channel = s.db.prepare('SELECT id FROM channels LIMIT 1').get().id;
      s.db.prepare('INSERT INTO messages (channel_id, user_id, attachment_hash, created_at) VALUES (?, ?, ?, 0)')
        .run(channel, user, kept);

      assert.deepEqual(s.chat.store(Buffer.from('twenty-five bytes, nearly'), 'text/plain'),
        { ok: false, error: 'server_full' });
      assert.ok(s.db.prepare('SELECT 1 FROM uploads WHERE hash = ?').get(kept), 'the row is still there');
      assert.ok(existsSync(s.chat.pathFor(kept)), 'and so is the file it promises');
    } finally {
      s.close();
    }
  });
});

describe('the per-file limit', () => {
  let s;
  before(() => { s = scratchChat(); });
  after(() => s.close());

  it('defaults to the one-request cap and never goes past 2 GB', () => {
    assert.equal(s.chat.maxFileBytes, MAX_UPLOAD_BYTES);
    s.chat.maxFileBytes = 10 * MAX_FILE_LIMIT;
    assert.equal(s.chat.maxFileBytes, MAX_FILE_LIMIT);
    s.chat.maxFileBytes = 'nonsense';
    assert.equal(s.chat.maxFileBytes, MAX_UPLOAD_BYTES);
  });

  it('caps one request by the lower of it and the one-request cap', () => {
    s.chat.maxFileBytes = MAX_FILE_LIMIT;
    assert.equal(s.chat.requestLimit(), MAX_UPLOAD_BYTES, 'a big limit is for pieces, not one body');
    s.chat.maxFileBytes = 4;
    assert.equal(s.chat.requestLimit(), 4);
    assert.equal(s.chat.requestLimit({ sealed: true }), 4 + 64, 'room for the sealing');
    assert.deepEqual(s.chat.store(Buffer.from('five!'), 'text/plain'), { ok: false, error: 'file_too_large' });
    s.chat.maxFileBytes = null;
  });
});

describe('upload sessions', () => {
  let s;
  let sessions;
  let clock = 0;
  before(() => {
    s = scratchChat();
    s.chat.maxFileBytes = 64;
    sessions = new UploadSessions(s.chat, { chunkBytes: 4, idleMs: 1000, now: () => clock });
  });
  after(() => s.close());

  it('puts a file together from its pieces, under the hash of all of it', async () => {
    const bytes = Buffer.from('0123456789');
    const opened = await sessions.open({ userId: 1, size: bytes.length, contentType: 'text/plain' });
    assert.equal(opened.ok, true);
    assert.equal(opened.chunkBytes, 4);
    for (let at = 0; at < bytes.length; at += 4) {
      const sent = await sessions.append({ id: opened.id, userId: 1, offset: at, body: body(bytes.subarray(at, at + 4)) });
      assert.deepEqual(sent, { ok: true, received: Math.min(bytes.length, at + 4) });
    }
    const done = await sessions.finish({ id: opened.id, userId: 1 });
    assert.equal(done.ok, true);
    assert.equal(done.upload.hash, sha256(bytes));
    assert.equal(done.upload.bytes, bytes.length);
    assert.equal(done.upload.content_type, 'text/plain');
    assert.equal(s.chat.usedBytes, bytes.length);
    assert.deepEqual(s.incoming(), [], 'nothing left half-arrived');
    assert.equal(sessions.size, 0);
  });

  it('says where it really is when a piece comes at the wrong offset', async () => {
    const opened = await sessions.open({ userId: 1, size: 8, contentType: 'text/plain' });
    await sessions.append({ id: opened.id, userId: 1, offset: 0, body: body(Buffer.from('abcd')) });
    // The same piece again, as a client that never saw the answer would send it.
    assert.deepEqual(
      await sessions.append({ id: opened.id, userId: 1, offset: 0, body: body(Buffer.from('abcd')) }),
      { ok: false, error: 'bad_offset', received: 4 },
    );
    assert.deepEqual(await sessions.finish({ id: opened.id, userId: 1 }), { ok: false, error: 'incomplete', received: 4 });
    assert.deepEqual(
      await sessions.append({ id: opened.id, userId: 1, offset: 4, body: body(Buffer.from('efgh')) }),
      { ok: true, received: 8 },
    );
    const done = await sessions.finish({ id: opened.id, userId: 1 });
    assert.equal(done.upload.hash, sha256(Buffer.from('abcdefgh')));
  });

  it('refuses a piece bigger than a piece, whether it says so or not', async () => {
    const opened = await sessions.open({ userId: 1, size: 6, contentType: 'text/plain' });
    assert.deepEqual(
      await sessions.append({ id: opened.id, userId: 1, offset: 0, body: body(Buffer.from('too long')) }),
      { ok: false, error: 'chunk_too_large' },
    );
    const { PassThrough } = await import('node:stream');
    const quiet = new PassThrough();
    const refused = sessions.append({ id: opened.id, userId: 1, offset: 0, body: quiet });
    quiet.write(Buffer.from('to'));
    quiet.write(Buffer.from('o long'));
    assert.deepEqual(await refused, { ok: false, error: 'chunk_too_large' });
    assert.equal(quiet.destroyed, false, 'the request still has an answer to carry');
    // Nor past the end of the file it said it was.
    await sessions.append({ id: opened.id, userId: 1, offset: 0, body: body(Buffer.from('1234')) });
    assert.deepEqual(
      await sessions.append({ id: opened.id, userId: 1, offset: 4, body: body(Buffer.from('5678')) }),
      { ok: false, error: 'chunk_too_large' },
    );
    await sessions.cancel({ id: opened.id, userId: 1 });
  });

  it('is nobody else\'s to add to, finish or cancel', async () => {
    const opened = await sessions.open({ userId: 1, size: 4, contentType: 'text/plain' });
    const other = { id: opened.id, userId: 2 };
    assert.deepEqual(await sessions.append({ ...other, offset: 0, body: body(Buffer.from('nope')) }),
      { ok: false, error: 'no_such_upload' });
    assert.deepEqual(await sessions.finish(other), { ok: false, error: 'no_such_upload' });
    await sessions.cancel(other);
    assert.deepEqual(await sessions.append({ id: opened.id, userId: 1, offset: 0, body: body(Buffer.from('mine')) }),
      { ok: true, received: 4 }, 'still there for its owner');
    await sessions.cancel({ id: opened.id, userId: 1 });
    assert.deepEqual(s.incoming(), []);
  });

  it(`holds ${PER_USER} open per account`, async () => {
    const mine = [];
    for (let i = 0; i < PER_USER; i += 1) {
      mine.push(await sessions.open({ userId: 3, size: 4, contentType: 'text/plain' }));
    }
    assert.ok(mine.every((r) => r.ok));
    assert.deepEqual(await sessions.open({ userId: 3, size: 4, contentType: 'text/plain' }),
      { ok: false, error: 'too_many_uploads' });
    const theirs = await sessions.open({ userId: 4, size: 4, contentType: 'text/plain' });
    assert.equal(theirs.ok, true, 'one person\'s uploads do not hold up another\'s');
    for (const { id } of mine) await sessions.cancel({ id, userId: 3 });
    await sessions.cancel({ id: theirs.id, userId: 4 });
  });

  it('holds no more room per account than one file may take', async () => {
    // 64 is the limit here: three files of 40 used to hold 120 between them.
    const first = await sessions.open({ userId: 5, size: 40, contentType: 'text/plain' });
    assert.equal(first.ok, true);
    assert.deepEqual(await sessions.open({ userId: 5, size: 40, contentType: 'text/plain' }),
      { ok: false, error: 'too_many_uploads' });
    const small = await sessions.open({ userId: 5, size: 24, contentType: 'text/plain' });
    assert.equal(small.ok, true, 'what still fits under the limit is fine');
    const theirs = await sessions.open({ userId: 6, size: 40, contentType: 'text/plain' });
    assert.equal(theirs.ok, true, 'and somebody else has their own');
    await sessions.cancel({ id: first.id, userId: 5 });
    const again = await sessions.open({ userId: 5, size: 40, contentType: 'text/plain' });
    assert.equal(again.ok, true, 'room given back is room to use again');
    for (const id of [small.id, again.id]) await sessions.cancel({ id, userId: 5 });
    await sessions.cancel({ id: theirs.id, userId: 6 });
  });

  it('keeps the allowlist and the limit', async () => {
    assert.deepEqual(await sessions.open({ userId: 1, size: 4, contentType: 'text/html' }),
      { ok: false, error: 'type_not_allowed' });
    assert.deepEqual(await sessions.open({ userId: 1, size: 65, contentType: 'text/plain' }),
      { ok: false, error: 'file_too_large' });
    assert.deepEqual(await sessions.open({ userId: 1, size: 0, contentType: 'text/plain' }),
      { ok: false, error: 'empty_file' });
  });

  it('drops a session nobody has sent to for a while, and its file', async () => {
    const opened = await sessions.open({ userId: 1, size: 8, contentType: 'text/plain' });
    await sessions.append({ id: opened.id, userId: 1, offset: 0, body: body(Buffer.from('half')) });
    assert.equal(s.incoming().length, 1);
    clock += 999;
    await sessions.sweep();
    assert.equal(sessions.size, 1, 'not yet');
    clock += 2;
    await sessions.sweep();
    assert.equal(sessions.size, 0);
    assert.deepEqual(s.incoming(), []);
    assert.deepEqual(await sessions.finish({ id: opened.id, userId: 1 }), { ok: false, error: 'no_such_upload' });
  });

  it('keeps the same file once', async () => {
    const bytes = Buffer.from('0123456789');
    const opened = await sessions.open({ userId: 1, size: 4, contentType: 'text/plain' });
    await sessions.append({ id: opened.id, userId: 1, offset: 0, body: body(bytes.subarray(0, 4)) });
    const first = await sessions.finish({ id: opened.id, userId: 1 });
    const used = s.chat.usedBytes;
    const again = await sessions.open({ userId: 1, size: 4, contentType: 'text/plain' });
    await sessions.append({ id: again.id, userId: 1, offset: 0, body: body(bytes.subarray(0, 4)) });
    const second = await sessions.finish({ id: again.id, userId: 1 });
    assert.equal(second.deduplicated, true);
    assert.equal(second.upload.hash, first.upload.hash);
    assert.equal(s.chat.usedBytes, used);
    assert.deepEqual(s.incoming(), [], 'the copy is gone');
  });

  it('never makes a private conversation\'s file public', async () => {
    const bytes = Buffer.from('sealed!!');
    assert.equal(s.chat.store(bytes, null, { sealed: true }).ok, true);
    const opened = await sessions.open({ userId: 1, size: 8, contentType: 'text/plain' });
    await sessions.append({ id: opened.id, userId: 1, offset: 0, body: body(bytes.subarray(0, 4)) });
    await sessions.append({ id: opened.id, userId: 1, offset: 4, body: body(bytes.subarray(4)) });
    assert.deepEqual(await sessions.finish({ id: opened.id, userId: 1 }), { ok: false, error: 'type_not_allowed' });
    assert.equal(s.chat.isSealed(sha256(bytes)), true);
    assert.deepEqual(s.incoming(), []);
  });
});

describe('upload sessions and the room they hold', () => {
  let s;
  let free;
  before(() => { s = scratchChat({ free: () => free, reserve: 100, quota: 1000 }); });
  after(() => s.close());

  it('holds room for the whole file from the start, and gives it back on cancel', async () => {
    s.chat.maxFileBytes = 1000;
    free = 100 + 600;
    const sessions = new UploadSessions(s.chat, { chunkBytes: 4 });
    const big = await sessions.open({ userId: 1, size: 500, contentType: 'text/plain' });
    assert.equal(big.ok, true);
    assert.deepEqual(await sessions.open({ userId: 2, size: 200, contentType: 'text/plain' }),
      { ok: false, error: 'disk_full' }, 'the disk is promised to the first');
    free = 10_000;
    assert.deepEqual(await sessions.open({ userId: 2, size: 600, contentType: 'text/plain' }),
      { ok: false, error: 'server_full' }, 'and so is the quota');
    await sessions.cancel({ id: big.id, userId: 1 });
    const now = await sessions.open({ userId: 2, size: 600, contentType: 'text/plain' });
    assert.equal(now.ok, true);
    await sessions.cancel({ id: now.id, userId: 2 });
  });
});

describe('a throttle spending part of a token', () => {
  it('lets fractions add up to a whole', () => {
    let now = 0;
    const t = new Throttle({ burst: 1, perSecond: 0.25, now: () => now });
    assert.equal(t.take('a', 0.5).allowed, true);
    assert.equal(t.take('a', 0.5).allowed, true);
    const third = t.take('a', 0.5);
    assert.equal(third.allowed, false);
    assert.equal(third.retryAfterSec, 2, 'as long as half a token takes to come back');
    now = 2000;
    assert.equal(t.take('a', 0.5).allowed, true);
  });
});

// ---------------------------------------------------------------------------
// Over HTTP

let fakeMtx;
let child;
let dataDir;
let ownerKey = null;
let ownerToken;
let memberToken;
let otherToken;

function startFakeMediaMtx() {
  return new Promise((done) => {
    fakeMtx = http.createServer((_req, res) => {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ itemCount: 0, pageCount: 0, items: [] }));
    });
    fakeMtx.listen(FAKE_MTX_PORT, '127.0.0.1', done);
  });
}

function startHarmony(extraEnv = {}) {
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
        // Whatever this machine's disk has free, the tests do not depend on it.
        HARMONY_DISK_RESERVE_BYTES: '0',
        ...extraEnv,
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

function stopHarmony() {
  return new Promise((done) => {
    if (!child || child.exitCode !== null) return done();
    child.once('exit', () => done());
    child.kill('SIGTERM');
    setTimeout(() => { child.kill('SIGKILL'); done(); }, 4000).unref();
  });
}

const api = async (path, { method = 'GET', body: json, token, raw, contentType } = {}) => {
  const res = await fetch(`${BASE}${path}`, {
    method,
    headers: {
      ...(json ? { 'Content-Type': 'application/json' } : {}),
      ...(raw ? { 'Content-Type': contentType ?? 'application/octet-stream' } : {}),
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    ...(json ? { body: JSON.stringify(json) } : {}),
    ...(raw ? { body: raw } : {}),
  });
  const text = await res.text();
  let parsed = null;
  try { parsed = JSON.parse(text); } catch { /* binary or empty */ }
  return { status: res.status, body: parsed, text, headers: res.headers };
};

const login = async (nickname) => (await api('/api/accounts/login', {
  method: 'POST', body: { nickname, password: 'hunter22' },
})).body.token;

describe('over HTTP', () => {
  before(async () => {
    dataDir = mkdtempSync(resolve(tmpdir(), '.harmony-uploads-'));
    await startFakeMediaMtx();
    await startHarmony();
    ownerToken = (await api('/api/accounts/register', {
      method: 'POST', body: { nickname: 'boss', password: 'hunter22', ownerKey },
    })).body.token;
    memberToken = (await api('/api/accounts/register', {
      method: 'POST', body: { nickname: 'member', password: 'hunter22' },
    })).body.token;
    otherToken = (await api('/api/accounts/register', {
      method: 'POST', body: { nickname: 'other', password: 'hunter22' },
    })).body.token;
  });

  after(async () => {
    await stopHarmony();
    await new Promise((done) => fakeMtx.close(done));
    rmSync(dataDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });

  describe('the largest file', () => {
    it('is told to every member with the server\'s settings', async () => {
      const res = await api('/api/server', { token: memberToken });
      assert.equal(res.body.server.maxFileBytes, MAX_UPLOAD_BYTES, 'the old cap, until the owner says');
      assert.equal(res.body.server.maxFileLimit, MAX_FILE_LIMIT);
    });

    it('is the owner\'s to change, within 1 MB and 2 GB', async () => {
      const member = await api('/api/server', { method: 'POST', body: { maxFileBytes: 64 * 1024 * 1024 }, token: memberToken });
      assert.equal(member.status, 403);
      for (const bad of [1024, MAX_FILE_LIMIT + 1, 'lots', true]) {
        const res = await api('/api/server', { method: 'POST', body: { maxFileBytes: bad }, token: ownerToken });
        assert.equal(res.status, 400, String(bad));
        assert.equal(res.body.error, 'bad_limit');
      }
      const set = await api('/api/server', { method: 'POST', body: { maxFileBytes: 40 * 1024 * 1024 }, token: ownerToken });
      assert.equal(set.status, 200);
      assert.equal(set.body.server.maxFileBytes, 40 * 1024 * 1024);
      const renamed = await api('/api/server', { method: 'POST', body: { name: 'Bloquinho' }, token: ownerToken });
      assert.equal(renamed.body.server.maxFileBytes, 40 * 1024 * 1024, 'left alone when not sent');
    });

    it('survives a restart, and goes back to the default with null', async () => {
      await stopHarmony();
      await startHarmony();
      [ownerToken, memberToken, otherToken] = [await login('boss'), await login('member'), await login('other')];
      assert.equal((await api('/api/server', { token: memberToken })).body.server.maxFileBytes, 40 * 1024 * 1024);
      const reset = await api('/api/server', { method: 'POST', body: { maxFileBytes: null }, token: ownerToken });
      assert.equal(reset.body.server.maxFileBytes, MAX_UPLOAD_BYTES);
      await api('/api/server', { method: 'POST', body: { maxFileBytes: 40 * 1024 * 1024 }, token: ownerToken });
    });

    it('caps a single POST too when it is lower than 25 MB', async () => {
      await api('/api/server', { method: 'POST', body: { maxFileBytes: 1024 * 1024 }, token: ownerToken });
      const res = await api('/api/uploads', {
        method: 'POST', raw: Buffer.alloc(1024 * 1024 + 1, 1), contentType: 'image/png', token: memberToken,
      });
      assert.equal(res.status, 413);
      assert.equal(res.body.error, 'file_too_large');
      await api('/api/server', { method: 'POST', body: { maxFileBytes: 40 * 1024 * 1024 }, token: ownerToken });
    });
  });

  describe('a file in pieces', () => {
    it('arrives whole, over the one-request cap, and downloads byte for byte', async () => {
      const bytes = randomBytes(MAX_UPLOAD_BYTES + 3 * 1024 * 1024 + 5);
      const opened = await api('/api/uploads/sessions', {
        method: 'POST', body: { size: bytes.length, contentType: 'video/mp4' }, token: memberToken,
      });
      assert.equal(opened.status, 201);
      const { id, chunkBytes } = opened.body;
      assert.equal(chunkBytes, 8 * 1024 * 1024);
      let at = 0;
      while (at < bytes.length) {
        const res = await api(`/api/uploads/sessions/${id}/chunk?offset=${at}`, {
          method: 'POST', raw: bytes.subarray(at, at + chunkBytes), token: memberToken,
        });
        assert.equal(res.status, 200, res.text);
        at = res.body.received;
      }
      const done = await api(`/api/uploads/sessions/${id}/finish`, { method: 'POST', token: memberToken });
      assert.equal(done.status, 201, done.text);
      assert.equal(done.body.hash, sha256(bytes));
      assert.equal(done.body.bytes, bytes.length);
      assert.equal(done.body.contentType, 'video/mp4');

      const back = await fetch(`${BASE}/api/uploads/${done.body.hash}`, { headers: { Authorization: `Bearer ${memberToken}` } });
      assert.deepEqual(Buffer.from(await back.arrayBuffer()), bytes);
      assert.deepEqual(readdirSync(resolve(dataDir, 'uploads', 'incoming')), [], 'nothing left half-arrived');

      // Its size goes with the message, so a client can offer it rather than load it.
      const channel = (await api('/api/channels', { token: memberToken })).body.channels.find((c) => c.kind === 'text');
      const posted = await api(`/api/channels/${channel.id}/messages`, {
        method: 'POST', body: { body: '', attachmentHash: done.body.hash, attachmentName: 'big.mp4' }, token: memberToken,
      });
      assert.equal(posted.status, 201, posted.text);
      assert.equal(posted.body.message.attachmentBytes, bytes.length);
      const history = await api(`/api/channels/${channel.id}/messages`, { token: memberToken });
      assert.equal(history.body.messages.find((m) => m.id === posted.body.message.id).attachmentBytes, bytes.length);
      const plain = await api(`/api/channels/${channel.id}/messages`, { method: 'POST', body: { body: 'no file' }, token: memberToken });
      assert.equal(plain.body.message.attachmentBytes, null);
    });

    it('answers a piece at the wrong place with the right one', async () => {
      const opened = await api('/api/uploads/sessions', {
        method: 'POST', body: { size: 10, contentType: 'text/plain' }, token: memberToken,
      });
      const { id } = opened.body;
      await api(`/api/uploads/sessions/${id}/chunk?offset=0`, { method: 'POST', raw: Buffer.from('01234'), token: memberToken });
      const again = await api(`/api/uploads/sessions/${id}/chunk?offset=0`, {
        method: 'POST', raw: Buffer.from('01234'), token: memberToken,
      });
      assert.equal(again.status, 409);
      assert.equal(again.body.error, 'bad_offset');
      assert.equal(again.body.received, 5);
      const early = await api(`/api/uploads/sessions/${id}/finish`, { method: 'POST', token: memberToken });
      assert.equal(early.status, 409);
      assert.equal(early.body.error, 'incomplete');
      await api(`/api/uploads/sessions/${id}/cancel`, { method: 'POST', token: memberToken });
    });

    it('is not somebody else\'s to touch', async () => {
      const { id } = (await api('/api/uploads/sessions', {
        method: 'POST', body: { size: 4, contentType: 'text/plain' }, token: memberToken,
      })).body;
      const chunk = await api(`/api/uploads/sessions/${id}/chunk?offset=0`, {
        method: 'POST', raw: Buffer.from('mine'), token: otherToken,
      });
      assert.equal(chunk.status, 404);
      assert.equal(chunk.body.error, 'no_such_upload');
      assert.equal((await api(`/api/uploads/sessions/${id}/finish`, { method: 'POST', token: otherToken })).status, 404);
      await api(`/api/uploads/sessions/${id}/cancel`, { method: 'POST', token: otherToken });
      const mine = await api(`/api/uploads/sessions/${id}/chunk?offset=0`, {
        method: 'POST', raw: Buffer.from('mine'), token: memberToken,
      });
      assert.equal(mine.status, 200, 'their cancel did nothing');
      assert.equal((await api(`/api/uploads/sessions/${id}/finish`, { method: 'POST', token: memberToken })).status, 201);
    });

    it('refuses a file over the limit, or of a type off the list, before a byte is sent', async () => {
      const big = await api('/api/uploads/sessions', {
        method: 'POST', body: { size: 40 * 1024 * 1024 + 1, contentType: 'video/mp4' }, token: memberToken,
      });
      assert.equal(big.status, 413);
      assert.equal(big.body.error, 'file_too_large');
      const html = await api('/api/uploads/sessions', {
        method: 'POST', body: { size: 10, contentType: 'text/html' }, token: memberToken,
      });
      assert.equal(html.status, 400);
      assert.equal(html.body.error, 'type_not_allowed');
    });

    it('needs an account', async () => {
      const res = await api('/api/uploads/sessions', { method: 'POST', body: { size: 10, contentType: 'text/plain' } });
      assert.equal(res.status, 401);
    });
  });

  describe('a nearly full disk', () => {
    it('refuses uploads of either kind with disk_full', async () => {
      await stopHarmony();
      // More free disk than any machine has: whatever this one has is short of it.
      await startHarmony({ HARMONY_DISK_RESERVE_BYTES: String(2 ** 60) });
      const token = await login('member');
      const one = await api('/api/uploads', { method: 'POST', raw: Buffer.from('hello'), contentType: 'text/plain', token });
      assert.equal(one.status, 507);
      assert.equal(one.body.error, 'disk_full');
      const pieces = await api('/api/uploads/sessions', {
        method: 'POST', body: { size: 10, contentType: 'text/plain' }, token,
      });
      assert.equal(pieces.status, 507);
      assert.equal(pieces.body.error, 'disk_full');
      assert.equal((await api('/api/health')).status, 200, 'and goes on running');
    });
  });
});
