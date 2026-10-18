// Opening a database another line of the server left behind.
//
//   node --test server/test/migrate.test.js

import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
import { MIGRATIONS, SCHEMA_VERSION, migrate } from '../src/db.js';

const columns = (db, table) => db.prepare(`PRAGMA table_info(${table})`).all().map((c) => c.name);
const version = (db) => db.prepare('PRAGMA user_version').all()[0].user_version;

describe('migrations', () => {
  it('bring an empty database to the current version', () => {
    const db = new DatabaseSync(':memory:');
    migrate(db);
    assert.equal(version(db), SCHEMA_VERSION);
    assert.ok(columns(db, 'channels').includes('mic_locked'));
    assert.ok(columns(db, 'messages').includes('reply_to'));
  });

  it("take Harmony 3.5's v11, replies, without losing them or the microphone lock", () => {
    const db = new DatabaseSync(':memory:');
    for (const step of MIGRATIONS.slice(0, 10)) step(db);
    db.exec('ALTER TABLE messages ADD COLUMN reply_to INTEGER');
    db.exec('PRAGMA user_version = 11');
    const user = db.prepare("INSERT INTO users (nickname, password_hash, created_at) VALUES ('ana', 'x', 0) RETURNING id").get().id;
    const channel = db.prepare("INSERT INTO channels (kind, name, position, created_at) VALUES ('text', 'chat', 99, 0) RETURNING id").get().id;
    db.prepare('INSERT INTO messages (channel_id, user_id, body, created_at) VALUES (?, ?, ?, 0)').run(channel, user, 'first');
    db.prepare('INSERT INTO messages (channel_id, user_id, body, reply_to, created_at) VALUES (?, ?, ?, 1, 0)').run(channel, user, 'answer');

    migrate(db);

    assert.equal(version(db), SCHEMA_VERSION);
    assert.ok(columns(db, 'channels').includes('mic_locked'));
    assert.deepEqual(db.prepare('SELECT body, reply_to FROM messages ORDER BY id').all().map((m) => ({ ...m })), [
      { body: 'first', reply_to: null },
      { body: 'answer', reply_to: 1 },
    ]);
  });

  it('index what deleting an upload or an account looks messages up by', () => {
    const db = new DatabaseSync(':memory:');
    for (const step of MIGRATIONS.slice(0, 16)) step(db);
    db.exec('PRAGMA user_version = 16');
    migrate(db);
    assert.equal(version(db), SCHEMA_VERSION);
    assert.ok(SCHEMA_VERSION >= 17);

    const plan = (sql) => db.prepare(`EXPLAIN QUERY PLAN ${sql}`).all().map((r) => r.detail).join(' | ');
    assert.match(plan('SELECT id FROM messages WHERE user_id = 1'), /USING (COVERING )?INDEX messages_user/);
    assert.match(plan("SELECT id FROM messages WHERE attachment_hash = 'x'"), /USING (COVERING )?INDEX messages_attachment/);
    assert.match(plan("SELECT id FROM dm_messages WHERE attachment_hash = 'x'"), /USING (COVERING )?INDEX dm_messages_attachment/);
  });

  it('leave a database of ours at v11 alone', () => {
    const db = new DatabaseSync(':memory:');
    for (const step of MIGRATIONS.slice(0, 11)) step(db);
    db.exec('PRAGMA user_version = 11');
    migrate(db);
    assert.equal(version(db), SCHEMA_VERSION);
  });
});