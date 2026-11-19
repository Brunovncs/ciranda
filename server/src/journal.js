// What the server writes down for looking back: who was in voice and for how long, what they
// were playing, and what Cirandinha played. Nothing here is read by the live server -- presence,
// rosters and activities stay derived from the sockets -- only by the year in review (retro.js).
//
// An open session is a row whose ended_at moves forward every minute, so a crash or a pulled
// plug loses at most that minute, and a restart leaves nothing to reconcile: the rows simply stop
// where the last beat left them.

/** How often open sessions are carried forward. */
const BEAT_MS = 60_000;
/** Shorter than this is a slip (a reconnect, a game that crashed at once) and not kept. */
const MIN_SESSION_MS = 30_000;

export class Journal {
  #db;
  #now;
  /** @type {Map<string, {id: number, startedAt: number}>} by `v:<user>:<channel>` or `a:<user>` */
  #open = new Map();
  #timer;

  constructor(db, { now = Date.now } = {}) {
    this.#db = db;
    this.#now = now;
    this.#timer = setInterval(() => this.beat(), BEAT_MS);
    this.#timer.unref?.();
  }

  // ------------------------------------------------------------------------- voice

  voiceJoined(channelId, userId) {
    this.#safe(() => this.#voiceJoined(channelId, userId));
  }

  #voiceJoined(channelId, userId) {
    const key = `v:${userId}:${channelId}`;
    if (this.#open.has(key)) return;
    const at = this.#now();
    const { lastInsertRowid } = this.#db
      .prepare('INSERT INTO voice_sessions (user_id, channel_id, started_at, ended_at) VALUES (?, ?, ?, ?)')
      .run(userId, channelId, at, at);
    this.#open.set(key, { id: Number(lastInsertRowid), startedAt: at });
  }

  voiceLeft(channelId, userId) {
    this.#safe(() => this.#close(`v:${userId}:${channelId}`, 'voice_sessions'));
  }

  // ------------------------------------------------------------------------- what they play

  /** `name` null when they stopped playing; a new name ends the last one first. */
  activity(userId, name) {
    this.#safe(() => this.#activity(userId, name));
  }

  #activity(userId, name) {
    const key = `a:${userId}`;
    const open = this.#open.get(key);
    if (open?.name === name) return;
    this.#close(key, 'activity_sessions');
    if (!name) return;
    const at = this.#now();
    const { lastInsertRowid } = this.#db
      .prepare('INSERT INTO activity_sessions (user_id, name, started_at, ended_at) VALUES (?, ?, ?, ?)')
      .run(userId, name, at, at);
    this.#open.set(key, { id: Number(lastInsertRowid), startedAt: at, name });
  }

  // ------------------------------------------------------------------------- Cirandinha

  played(channelId, track) {
    this.#safe(() => this.#db
      .prepare('INSERT INTO music_plays (channel_id, source_key, title, artist, added_by, played_at) VALUES (?, ?, ?, ?, ?, ?)')
      .run(channelId, String(track.source ?? ''), track.title ?? '', track.artist ?? '', track.addedBy ?? null, this.#now()));
  }

  // -------------------------------------------------------------------------

  /** Carries every open session forward to now. */
  beat() {
    this.#safe(() => this.#beat());
  }

  #beat() {
    const at = this.#now();
    const voice = this.#db.prepare('UPDATE voice_sessions SET ended_at = ? WHERE id = ?');
    const activity = this.#db.prepare('UPDATE activity_sessions SET ended_at = ? WHERE id = ?');
    for (const [key, open] of this.#open) (key.startsWith('v:') ? voice : activity).run(at, open.id);
  }

  /** Ends every open session now, for a server shutting down. */
  closeAll() {
    for (const key of [...this.#open.keys()]) this.#close(key, key.startsWith('v:') ? 'voice_sessions' : 'activity_sessions');
    clearInterval(this.#timer);
  }

  /** The journal is never worth failing what it watches: a voice join, a song. */
  #safe(fn) {
    try {
      fn();
    } catch (err) {
      console.warn(`[journal] ${err.message}`);
    }
  }

  #close(key, table) {
    const open = this.#open.get(key);
    if (!open) return;
    this.#open.delete(key);
    const at = this.#now();
    // Not interpolated from outside: `table` is one of two literals above.
    if (at - open.startedAt < MIN_SESSION_MS) this.#db.prepare(`DELETE FROM ${table} WHERE id = ?`).run(open.id);
    else this.#db.prepare(`UPDATE ${table} SET ended_at = ? WHERE id = ?`).run(at, open.id);
  }
}
