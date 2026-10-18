// Big uploads, in pieces, straight to disk.
//
// POST /api/uploads takes a file as one request body -- fine for a picture,
// and the reason it stays capped at 25 MB. A 2 GB file that way would be one
// request that has to survive however long a home upload takes, and one
// failure near the end would mean sending all of it again.
//
// So a big file is a session instead:
//
//   POST /api/uploads/sessions            { size, contentType } -> { id, chunkBytes }
//   POST /api/uploads/sessions/:id/chunk  ?offset=N, raw body   -> { received }
//   POST /api/uploads/sessions/:id/finish                       -> the upload
//   POST /api/uploads/sessions/:id/cancel
//
// The same routes, shapes and codes as the Electron line's servers, so either
// line's client can send to either line's server.
//
// Each chunk is at most chunkBytes (8 MB): small enough to hold whole, so a
// chunk is written and hashed only once all of it has arrived, and one that
// breaks halfway is simply sent again from the same offset. A chunk sent at
// the wrong offset -- a retry of one that did arrive after all -- is answered
// with where the file really is, and the client carries on from there. 8 MB
// also keeps every request far under the body limits proxies and tunnels
// impose (Cloudflare's is 100 MB).
//
// The file is hashed as it arrives, so finishing is a rename into the
// content-addressed store (Chat.adopt) rather than a second read of 2 GB --
// through the same checks as any other upload: the type allowlist, the quota,
// and a hash already kept as a private conversation's. Room for the whole
// file, quota and disk, is held from the moment the session opens.
//
// Sessions live in memory: a restart forgets them, and Chat clears the
// directory they write to when it starts. Private conversations do not use
// them: their files are sealed whole on the client, and come in one request.

import { createHash, randomBytes } from 'node:crypto';
import { open, unlink } from 'node:fs/promises';
import { join } from 'node:path';

import { mediaTypeOf } from './chat.js';

export const CHUNK_BYTES = 8 * 1024 * 1024;
/** A session nothing has been sent to for this long is abandoned. */
const IDLE_MS = 10 * 60 * 1000;
/**
 * Open sessions per account: enough for a few files at once, not a flood.
 * Together they may hold no more than one file's limit; see open().
 */
export const PER_USER = 3;

/**
 * The whole of one chunk, or why not. The source is never destroyed, as in
 * Chat.receive: whoever passed it still has an answer to give on the same
 * connection. A chunk over `max` is refused the moment it gets there.
 */
function readChunk(source, max) {
  return new Promise((done) => {
    const parts = [];
    let length = 0;
    let settled = false;
    const settle = (result) => {
      if (settled) return;
      settled = true;
      source.off('data', onData);
      source.off('end', onEnd);
      source.off('error', onGone);
      source.off('close', onClose);
      done(result);
    };
    const onData = (part) => {
      length += part.length;
      if (length > max) {
        source.pause();
        settle({ ok: false, error: 'chunk_too_large' });
      } else {
        parts.push(part);
      }
    };
    const onEnd = () => settle(length
      ? { ok: true, chunk: Buffer.concat(parts, length) }
      : { ok: false, error: 'empty_file' });
    const onGone = () => settle({ ok: false, error: 'aborted' });
    const onClose = () => { if (!source.readableEnded) onGone(); };
    source.on('data', onData);
    source.on('end', onEnd);
    source.on('error', onGone);
    source.on('close', onClose);
  });
}

export class UploadSessions {
  #chat;
  #chunkBytes;
  #idleMs;
  #now;
  /** @type {Map<string, object>} */
  #sessions = new Map();

  /**
   * @param {import('./chat.js').Chat} chat
   */
  constructor(chat, { chunkBytes = CHUNK_BYTES, idleMs = IDLE_MS, now = Date.now } = {}) {
    this.#chat = chat;
    this.#chunkBytes = chunkBytes;
    this.#idleMs = idleMs;
    this.#now = now;
    const sweeper = setInterval(() => this.sweep(), 60 * 1000);
    sweeper.unref();
  }

  get size() {
    return this.#sessions.size;
  }

  async open({ userId, size, contentType }) {
    if (!Number.isSafeInteger(size) || size <= 0) return { ok: false, error: 'empty_file' };
    if (size > this.#chat.maxFileBytes) return { ok: false, error: 'file_too_large' };
    const type = String(contentType ?? '').split(';')[0].trim().toLowerCase();
    if (!mediaTypeOf(type)) return { ok: false, error: 'type_not_allowed' };
    const mine = [...this.#sessions.values()].filter((s) => s.userId === userId);
    if (mine.length >= PER_USER) return { ok: false, error: 'too_many_uploads' };
    /*
     * And no more room between them than the largest one file may take.
     * Each session holds its whole size from the start, so three at the
     * 2 GB limit let one member hold 6 GB of a server's room for as long
     * as they cared to dribble bytes in, and everybody else was told the
     * server was full. A few small files at once still fit.
     */
    const held = mine.reduce((sum, s) => sum + s.size, 0);
    if (held + size > this.#chat.maxFileBytes) return { ok: false, error: 'too_many_uploads' };
    // Room for all of it, on top of every other upload still arriving.
    const room = this.#chat.reserve(size);
    if (!room.ok) return room;

    const id = randomBytes(16).toString('hex');
    const path = join(this.#chat.incomingDir, `session-${id}`);
    let handle;
    try {
      handle = await open(path, 'w');
    } catch (err) {
      room.release();
      throw err;
    }
    this.#sessions.set(id, {
      id,
      userId,
      size,
      contentType: type,
      path,
      handle,
      release: room.release,
      hash: createHash('sha256'),
      received: 0,
      busy: false,
      touched: this.#now(),
    });
    return { ok: true, id, chunkBytes: this.#chunkBytes };
  }

  /** The session, if it is this account's. Somebody else's is answered as if it did not exist. */
  #mine(id, userId) {
    const session = this.#sessions.get(id);
    return session && session.userId === userId ? session : null;
  }

  /**
   * One chunk, at the offset the session has reached.
   *
   * @param {{id: string, userId: number, offset: number,
   *          body: import('node:stream').Readable & {headers?: object}}} args
   */
  async append({ id, userId, offset, body }) {
    const session = this.#mine(id, userId);
    if (!session) return { ok: false, error: 'no_such_upload' };
    // A retry of a chunk that did arrive, or a second request racing the
    // first: either way, say where the file really is.
    if (session.busy || offset !== session.received) {
      return { ok: false, error: 'bad_offset', received: session.received };
    }

    const max = Math.min(this.#chunkBytes, session.size - session.received);
    const declared = Number(body.headers?.['content-length']);
    if (Number.isFinite(declared) && declared > max) return { ok: false, error: 'chunk_too_large' };

    session.busy = true;
    try {
      const read = await readChunk(body, max);
      if (!read.ok) return read;
      // Positional and asynchronous: 8 MB to an SD card is long enough that
      // a synchronous write would stall every other request on the server.
      await session.handle.write(read.chunk, 0, read.chunk.length, session.received);
      session.hash.update(read.chunk);
      session.received += read.chunk.length;
      return { ok: true, received: session.received };
    } finally {
      session.busy = false;
      session.touched = this.#now();
    }
  }

  async finish({ id, userId }) {
    const session = this.#mine(id, userId);
    if (!session) return { ok: false, error: 'no_such_upload' };
    if (session.busy || session.received !== session.size) {
      return { ok: false, error: 'incomplete', received: session.received };
    }
    this.#sessions.delete(id);
    try {
      await session.handle.close();
      return this.#chat.adopt(session.path, {
        hash: session.hash.digest('hex'),
        bytes: session.size,
        contentType: session.contentType,
      });
    } catch (err) {
      await unlink(session.path).catch(() => {});
      throw err;
    } finally {
      session.release();
    }
  }

  async cancel({ id, userId }) {
    const session = this.#mine(id, userId);
    if (session) await this.#drop(session);
    return { ok: true };
  }

  async #drop(session) {
    this.#sessions.delete(session.id);
    session.release();
    await session.handle.close().catch(() => {});
    await unlink(session.path).catch(() => {});
  }

  /** Forget sessions nobody has sent anything to for a while, and their files. */
  async sweep() {
    const cutoff = this.#now() - this.#idleMs;
    for (const session of [...this.#sessions.values()]) {
      if (!session.busy && session.touched < cutoff) await this.#drop(session);
    }
  }
}
