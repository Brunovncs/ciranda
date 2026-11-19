// The journal (hours in voice, games, songs) and the year in review built on it.
//
//   node --test server/test/retro.test.js

import { after, describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { openDatabase } from '../src/db.js';
import { Journal } from '../src/journal.js';
import { yearInReview, overlaps, longestHangout } from '../src/retro.js';
import { cleanActivity } from '../src/realtime.js';
import { VoiceRooms } from '../src/channels.js';

const MIN = 60_000;
const HOUR = 60 * MIN;
const made = [];
// Closed first: Windows will not delete a database that is still open.
after(() => made.forEach(({ dir, db }) => {
  db.close();
  rmSync(dir, { recursive: true, force: true });
}));

function freshDb() {
  const dir = mkdtempSync(join(tmpdir(), 'ciranda-retro-'));
  const db = openDatabase({ dataDir: dir });
  made.push({ dir, db });
  const user = db.prepare("INSERT INTO users (nickname, password_hash, role, created_at) VALUES (?, 'x', 'member', 0)");
  const ids = ['ana', 'bia', 'caio'].map((n) => Number(user.run(n).lastInsertRowid));
  const channel = db.prepare("INSERT INTO channels (kind, name, position, created_at) VALUES (?, ?, 0, 0)");
  const text = Number(channel.run('text', 'geral').lastInsertRowid);
  const voice = Number(channel.run('voice', 'resenha').lastInsertRowid);
  return { db, ids, text, voice };
}

/** A clock the test moves by hand. */
function clock(at) {
  const c = { at, now: () => c.at };
  return c;
}

describe('journal', () => {
  it('keeps a voice session from join to leave, and drops one too short to be real', () => {
    const { db, ids: [ana, bia], voice } = freshDb();
    const t = clock(Date.UTC(2026, 10, 20, 20));
    const journal = new Journal(db, { now: t.now });
    journal.voiceJoined(voice, ana);
    journal.voiceJoined(voice, bia);
    t.at += 10_000;
    journal.voiceLeft(voice, bia); // ten seconds: a slip
    t.at += 2 * HOUR;
    journal.voiceLeft(voice, ana);
    const rows = db.prepare('SELECT user_id, started_at, ended_at FROM voice_sessions').all();
    assert.equal(rows.length, 1);
    assert.equal(rows[0].user_id, ana);
    assert.equal(rows[0].ended_at - rows[0].started_at, 2 * HOUR + 10_000);
    journal.closeAll();
  });

  it('carries an open session forward on each beat, so a crash loses only the last minute', () => {
    const { db, ids: [ana], voice } = freshDb();
    const t = clock(1_000_000);
    const journal = new Journal(db, { now: t.now });
    journal.voiceJoined(voice, ana);
    t.at += 5 * MIN;
    journal.beat();
    const row = db.prepare('SELECT started_at, ended_at FROM voice_sessions').get();
    assert.equal(row.ended_at - row.started_at, 5 * MIN);
    journal.closeAll();
  });

  it('ends a game when another starts or when it stops', () => {
    const { db, ids: [ana] } = freshDb();
    const t = clock(0);
    const journal = new Journal(db, { now: t.now });
    journal.activity(ana, 'VALORANT');
    t.at += HOUR;
    journal.activity(ana, 'VALORANT'); // said again: nothing new
    journal.activity(ana, 'Enshrouded');
    t.at += 30 * MIN;
    journal.activity(ana, null);
    const rows = db.prepare('SELECT name, ended_at - started_at AS ms FROM activity_sessions ORDER BY id').all();
    assert.deepEqual(rows.map((r) => [r.name, r.ms]), [['VALORANT', HOUR], ['Enshrouded', 30 * MIN]]);
    journal.closeAll();
  });

  it('hears every way out of a voice room through VoiceRooms', () => {
    const { db, ids: [ana, bia], voice } = freshDb();
    const t = clock(0);
    const journal = new Journal(db, { now: t.now });
    const rooms = new VoiceRooms({ onJoin: (c, u) => journal.voiceJoined(c, u), onLeave: (c, u) => journal.voiceLeft(c, u) });
    rooms.join(voice, { id: ana, nickname: 'ana' });
    rooms.join(voice, { id: bia, nickname: 'bia' });
    rooms.join(voice, { id: ana, nickname: 'ana' }); // a reconnect keeps the slot and the session
    t.at += HOUR;
    rooms.leaveAll(ana);
    rooms.closeRoom(voice);
    const rows = db.prepare('SELECT user_id, ended_at - started_at AS ms FROM voice_sessions ORDER BY user_id').all();
    assert.deepEqual(rows.map((r) => [r.user_id, r.ms]), [[ana, HOUR], [bia, HOUR]]);
    journal.closeAll();
  });
});

describe('activity names', () => {
  it('takes a trimmed name, nothing for null or blank, and refuses what cannot be one', () => {
    assert.equal(cleanActivity('  VALORANT '), 'VALORANT');
    assert.equal(cleanActivity(null), null);
    assert.equal(cleanActivity('   '), null);
    assert.equal(cleanActivity(42), undefined);
    assert.equal(cleanActivity('x'.repeat(65)), undefined);
    assert.equal(cleanActivity('evil‮game'), undefined);
  });
});

describe('year in review', () => {
  it('pairs people by the time they spent in one channel together', () => {
    const pairs = overlaps([
      { userId: 1, channelId: 9, a: 0, b: 3 * HOUR },
      { userId: 2, channelId: 9, a: HOUR, b: 4 * HOUR },
      { userId: 3, channelId: 8, a: 0, b: 4 * HOUR }, // another channel: with nobody
    ]);
    assert.deepEqual(pairs, [{ a: 1, b: 2, ms: 2 * HOUR }]);
  });

  it('finds the longest stretch with two or more, through hand-overs', () => {
    const best = longestHangout([
      { userId: 1, channelId: 9, a: 0, b: 2 * HOUR },
      { userId: 2, channelId: 9, a: HOUR, b: 5 * HOUR },
      { userId: 3, channelId: 9, a: 2 * HOUR, b: 3 * HOUR }, // takes over from 1 as 1 leaves
    ]);
    assert.equal(best.startedAt, HOUR);
    assert.equal(best.ms, 2 * HOUR);
    assert.deepEqual(best.people.sort(), [1, 2, 3]);
  });

  it("counts the server's year and the asker's own, in the asker's time", () => {
    const { db, ids: [ana, bia, caio], text, voice } = freshDb();
    const at = (h) => Date.UTC(2026, 9, 10, h); // 10 Oct 2026, UTC
    const msg = db.prepare('INSERT INTO messages (channel_id, user_id, body, created_at) VALUES (?, ?, ?, ?)');
    msg.run(text, ana, 'bom dia', at(12));
    msg.run(text, ana, 'alguém online?', at(13));
    msg.run(text, bia, 'eu', at(13));
    msg.run(text, caio, 'boa noite', at(4)); // 01:00 in São Paulo: the night owl
    msg.run(text, caio, 'ano passado', Date.UTC(2025, 5, 1)); // not this year
    const react = db.prepare('INSERT INTO message_reactions (message_id, user_id, emoji, created_at) VALUES (1, ?, ?, ?)');
    react.run(bia, '🔥', at(14));
    react.run(caio, '🔥', at(14));
    react.run(ana, '😂', at(14));
    const v = db.prepare('INSERT INTO voice_sessions (user_id, channel_id, started_at, ended_at) VALUES (?, ?, ?, ?)');
    v.run(ana, voice, at(20), at(23));
    v.run(bia, voice, at(21), at(23));
    db.prepare('INSERT INTO activity_sessions (user_id, name, started_at, ended_at) VALUES (?, ?, ?, ?)').run(ana, 'VALORANT', at(20), at(22));
    db.prepare("INSERT INTO music_plays (channel_id, source_key, title, artist, added_by, played_at) VALUES (?, 'yt:1', 'Evidências', 'Chitãozinho & Xororó', 'bia', ?)").run(voice, at(21));

    const r = yearInReview(db, { year: 2026, tzMinutes: -180, userId: ana, now: Date.UTC(2026, 11, 31) });
    assert.equal(r.server.messages, 4);
    assert.equal(r.server.writers, 3);
    assert.deepEqual(r.server.topChatters[0], { userId: ana, n: 2 });
    assert.equal(r.server.busiestChannel.name, 'geral');
    assert.deepEqual(r.server.nightOwl, { userId: caio, n: 1 });
    assert.deepEqual(r.server.topReactions[0], { emoji: '🔥', n: 2 });
    assert.equal(r.server.voiceMs, 5 * HOUR);
    assert.deepEqual(r.server.topPair, { a: ana, b: bia, ms: 2 * HOUR });
    assert.equal(r.server.longestHangout.ms, 2 * HOUR);
    assert.deepEqual(r.server.games[0], { name: 'VALORANT', ms: 2 * HOUR, players: 1 });
    assert.deepEqual(r.server.topSongs[0], { title: 'Evidências', artist: 'Chitãozinho & Xororó', n: 1 });
    assert.deepEqual(r.server.topDj, { nickname: 'bia', n: 1 });
    assert.equal(r.server.byHour[1], 1); // the night owl's message, at 01:00 local

    assert.equal(r.me.messages, 2);
    assert.equal(r.me.rank, 1);
    assert.equal(r.me.firstMessage.body, 'bom dia');
    assert.equal(r.me.voiceMs, 3 * HOUR);
    assert.deepEqual(r.me.partner, { userId: bia, ms: 2 * HOUR });
    assert.deepEqual(r.me.topGame, { name: 'VALORANT', ms: 2 * HOUR });
    assert.deepEqual(r.me.topReaction, { emoji: '😂', n: 1 });
  });

  it("leaves out a banned account's share", () => {
    const { db, ids: [ana, bia], text } = freshDb();
    const msg = db.prepare('INSERT INTO messages (channel_id, user_id, body, created_at) VALUES (?, ?, ?, ?)');
    msg.run(text, ana, 'oi', Date.UTC(2026, 3, 1));
    msg.run(text, bia, 'oi', Date.UTC(2026, 3, 1));
    db.prepare('UPDATE users SET banned_at = 1 WHERE id = ?').run(bia);
    const r = yearInReview(db, { year: 2026, tzMinutes: 0, userId: ana, now: Date.UTC(2026, 11, 31) });
    assert.equal(r.server.messages, 1);
  });
});
