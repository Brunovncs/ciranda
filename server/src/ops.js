// The operations API: /ops/*, for an admin panel running on the same machine.
//
// A home lab runs several of these servers side by side, one container each,
// and a panel beside them looks after all of them -- who is on, who is banned,
// the door password, a backup. It is the host's tool, not a member's, so it
// sits outside everything a member goes through: no door password, no account,
// no role. What it has instead is three checks, all of which must pass:
//
//   - the connection comes from this machine (127.0.0.1 or ::1);
//   - no proxy header is present, so it did not arrive through Caddy or a
//     tunnel connector, which also connect from this machine;
//   - it carries HARMONY_OPS_TOKEN as a bearer token.
//
// Caddy forwards only /api/* and /ws here, so the second check should never be
// what stops anybody. It is there for the day somebody widens that.
//
// Anything that fails gets the same 404 as a path that does not exist, so a
// stranger cannot even tell the API is here, let alone which check failed.

import { randomBytes } from 'node:crypto';
import { createReadStream, readdirSync, readFileSync, rmSync, statSync } from 'node:fs';
import { rm } from 'node:fs/promises';
import { join } from 'node:path';
import { pipeline } from 'node:stream/promises';
import express from 'express';

import { secretsMatch } from './auth.js';
import { issueOwnerToken, publicUser } from './accounts.js';

/**
 * Shorter than this is refused outright. The token is the whole of the
 * protection once something on the machine can reach the port, so a short
 * one is turned down rather than quietly accepted.
 */
const MIN_TOKEN = 24;

/** Exactly what a loopback peer looks like to Node, IPv4-mapped included. */
const LOOPBACK = new Set(['127.0.0.1', '::1', '::ffff:127.0.0.1']);

/** A snapshot being written or sent; a crash mid-way leaves one behind. */
const BACKUP_PREFIX = '.ops-backup-';

/**
 * Mount the routes, or do nothing when no usable token is configured.
 *
 * @param {import('express').Express} app
 * @param {object} deps
 * @param {string} deps.token HARMONY_OPS_TOKEN
 * @param {string|null} deps.build HARMONY_BUILD
 * @param {import('node:sqlite').DatabaseSync} deps.db
 * @param {string} deps.dataDir
 * @param {string} deps.dbPath
 * @param {import('./accounts.js').Accounts} deps.accounts
 * @param {{get: (k: string) => string|null}} deps.metaStore
 * @param {import('./server-settings.js').ServerSettings} deps.serverSettings
 * @param {import('./chat.js').Chat} deps.chat
 * @param {import('./channels.js').Channels} deps.channels
 * @param {import('./channels.js').VoiceRooms} deps.voice
 * @param {import('./calls.js').Calls} deps.calls
 * @param {import('./auth.js').LoginLimiter} deps.loginLimiter
 * @param {() => import('./realtime.js').Realtime|null} deps.realtime
 * @param {() => object} deps.serverView
 * @param {(res: import('express').Response, id: number, by: string) => void} deps.removeAccount
 * @param {(password: string) => void} deps.setDoorPassword
 * @returns {boolean} whether it is on
 */
export function mountOps(app, deps) {
  const { token } = deps;
  if (!token) return false;
  if (token.length < MIN_TOKEN) {
    console.warn(`[ops] HARMONY_OPS_TOKEN is shorter than ${MIN_TOKEN} characters; the operations API stays off`);
    return false;
  }

  const {
    db, dataDir, dbPath, accounts, metaStore, serverSettings, chat, channels, voice, calls,
    loginLimiter, realtime, serverView, removeAccount, setDoorPassword,
  } = deps;
  const { version } = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'));
  const startedAt = Date.now();
  sweepBackups(dataDir);

  const router = express.Router();

  // next('router') leaves this router altogether, so a refusal is answered
  // by whatever answers an unknown path: the same 404, byte for byte.
  router.use((req, _res, next) => (allowed(req, token) ? next() : next('router')));

  const online = () => new Set(realtime()?.onlineUserIds() ?? []);
  const idOf = (req) => Number.parseInt(req.params.id, 10);
  const noSuchUser = (res) => res.status(404).json({ error: 'no_such_user', message: 'No such account.' });
  const everyone = () => realtime()?.broadcast({ type: 'accounts', users: accounts.list().map(publicUser) });

  router.get('/status', (_req, res) => {
    const nicknameOf = (id) => accounts.byId(id)?.nickname ?? null;
    const voiceRooms = Object.entries(voice.allRosters())
      .filter(([, roster]) => roster.length > 0)
      .map(([channelId, roster]) => ({
        channelId: Number(channelId),
        channelName: channels.get(Number(channelId))?.name ?? null,
        members: roster.map((m) => ({
          id: m.userId, nickname: m.nickname, muted: m.muted, deafened: m.deafened,
        })),
      }));
    res.json({
      name: serverSettings.name,
      version,
      build: deps.build ?? null,
      startedAt,
      uptimeSec: Math.floor((Date.now() - startedAt) / 1000),
      schema: db.prepare('PRAGMA user_version').get().user_version,
      ownerClaimed: accounts.hasOwner,
      accounts: accounts.count,
      banned: accounts.bannedCount,
      online: [...online()].map((id) => ({ id, nickname: nicknameOf(id) })),
      voice: voiceRooms,
      // A count and nothing more: who is calling whom is the two of theirs.
      calls: calls.activeCount,
      storage: {
        usedBytes: chat.usedBytes,
        quotaBytes: chat.quotaBytes,
        diskFreeBytes: chat.diskFreeBytes,
        maxFileBytes: chat.maxFileBytes,
      },
      dbBytes: databaseBytes(dbPath),
    });
  });

  router.get('/accounts', (_req, res) => {
    const on = online();
    res.json(accounts.listAll().map((u) => opsAccount(u, on)));
  });

  /*
   * The owner's own route, less the one power the host has no business
   * with: ownership moves by the owner key, not from here.
   */
  router.post('/accounts/:id/role', (req, res) => {
    const role = String(req.body?.role ?? '');
    if (role !== 'admin' && role !== 'member') {
      return res.status(400).json({ error: 'invalid_role', message: 'Role must be admin or member.' });
    }
    const target = accounts.byId(idOf(req));
    if (!target) return noSuchUser(res);
    if (target.role === 'owner') {
      return res.status(403).json({
        error: 'cannot_change_owner',
        message: 'The owner keeps the role. Reset the owner key to hand the server over.',
      });
    }
    const { user } = accounts.setRole(target.id, role);
    // Nobody else knows a banned account is there, so there is nobody to tell.
    if (user.banned_at == null) realtime()?.broadcast({ type: 'user:updated', user: publicUser(user) });
    return res.json({ ok: true, user: opsAccount(user, online()) });
  });

  /** Signed out everywhere, sockets included. Nothing stops them signing back in. */
  router.post('/accounts/:id/kick', (req, res) => {
    const target = accounts.byId(idOf(req));
    if (!target) return noSuchUser(res);
    accounts.endSessions(target.id);
    realtime()?.closeUser(target.id);
    console.warn(`[ops] ${target.nickname} signed out by the host`);
    return res.json({ ok: true, user: opsAccount(target, online()) });
  });

  /**
   * A new password for somebody who lost theirs. Every session they had
   * ends, as it does when they change it themselves, and so does any lockout
   * their guessing earned: the right password now is this one.
   */
  router.post('/accounts/:id/password', async (req, res) => {
    const target = accounts.byId(idOf(req));
    if (!target) return noSuchUser(res);
    const result = await accounts.changePassword(target.id, req.body?.password);
    if (!result.ok) {
      return res.status(400).json({ error: result.error, message: 'Use at least 6 characters.' });
    }
    realtime()?.closeUser(target.id);
    loginLimiter.succeed(target.nickname);
    console.warn(`[ops] ${target.nickname}'s password reset by the host`);
    return res.json({ ok: true, user: opsAccount(accounts.byId(target.id), online()) });
  });

  /*
   * Banned: signed out, refused at sign-in, gone from everybody's member
   * list. A call they were in ends there and then rather than waiting out
   * the grace a dropped connection gets.
   */
  router.post('/accounts/:id/ban', (req, res) => {
    const result = accounts.ban(idOf(req));
    if (!result.ok) {
      if (result.error === 'no_such_user') return noSuchUser(res);
      return res.status(403).json({ error: result.error, message: 'The owner cannot be banned.' });
    }
    calls.leaveUser(result.user.id);
    realtime()?.closeUser(result.user.id);
    everyone();
    console.warn(`[ops] ${result.user.nickname} banned by the host`);
    return res.json({ ok: true, user: opsAccount(result.user, online()) });
  });

  router.post('/accounts/:id/unban', (req, res) => {
    const result = accounts.unban(idOf(req));
    if (!result.ok) return noSuchUser(res);
    everyone();
    console.warn(`[ops] ${result.user.nickname} unbanned by the host`);
    return res.json({ ok: true, user: opsAccount(result.user, online()) });
  });

  router.post('/accounts/:id/delete', (req, res) => removeAccount(res, idOf(req), 'the host'));

  router.post('/server', (req, res) => {
    if (typeof req.body?.name !== 'string') {
      return res.status(400).json({ error: 'bad_name', message: 'Send the name as a string.' });
    }
    serverSettings.setName(req.body.name);
    const view = serverView();
    realtime()?.broadcast({ type: 'server', server: view });
    return res.json({ server: view });
  });

  /*
   * The door password, live. The host writes the same value into the env
   * file, which is what the next boot reads; this saves waiting for it.
   * Sessions stay: the people inside already came through the door.
   */
  router.post('/server-password', (req, res) => {
    const password = req.body?.password;
    if (password !== null && typeof password !== 'string') {
      return res.status(400).json({ error: 'bad_password', message: 'Send a string, or null for no password.' });
    }
    setDoorPassword(password ?? '');
    const view = serverView();
    realtime()?.broadcast({ type: 'server', server: view });
    console.warn(`[ops] door password ${view.passwordRequired ? 'changed' : 'removed'} by the host`);
    return res.json({ ok: true, passwordRequired: view.passwordRequired });
  });

  router.get('/owner-key', (_req, res) => {
    const claimed = accounts.hasOwner;
    res.json({ claimed, key: claimed ? null : metaStore.get('owner_token') ?? issueOwnerToken(metaStore) });
  });

  /**
   * Make the server claimable again, for an owner who has lost their
   * account or is handing over to someone else.
   *
   * Whoever owns it now becomes an admin -- not a member, since nothing they
   * did is undone -- and the first person to present the new key owns it.
   * Until then there is no owner, which is the state a new server is in and
   * which every route already handles. Asked for in so many words, because
   * it is the one thing here that takes power away from a person.
   */
  router.post('/owner-key/reset', (req, res) => {
    if (req.body?.confirm !== true) {
      return res.status(400).json({ error: 'confirm_required', message: 'Send {"confirm": true} to reset the owner key.' });
    }
    const demoted = accounts.demoteOwners();
    const key = issueOwnerToken(metaStore);
    for (const id of demoted) realtime()?.broadcast({ type: 'user:updated', user: publicUser(accounts.byId(id)) });
    console.warn(`[ops] owner key reset by the host; ${demoted.length} owner(s) now admin, unclaimed until the key is used`);
    return res.json({ key });
  });

  /**
   * A consistent copy of the database, streamed.
   *
   * VACUUM INTO writes a snapshot as of one moment however busy the server
   * is, which copying the file would not be: the WAL holds what the main
   * file does not yet. It runs on the event loop, which for a friends'
   * server is a few megabytes and a blink. The uploads are not in it; they
   * are files, and the host can copy those as they are.
   */
  router.get('/backup', async (_req, res) => {
    const file = join(dataDir, `${BACKUP_PREFIX}${randomBytes(8).toString('hex')}.db`);
    try {
      db.prepare('VACUUM INTO ?').run(file);
    } catch (err) {
      rmSync(file, { force: true });
      throw err;
    }
    const stamp = new Date().toISOString().replace(/[:.]/g, '-');
    res.set('Content-Type', 'application/octet-stream');
    res.set('Content-Length', String(statSync(file).size));
    res.set('Content-Disposition', `attachment; filename="harmony-${stamp}.db"`);
    const source = createReadStream(file);
    // Deleted once the file is closed, not when the pipe ends: Windows will
    // not delete a file that is still open.
    source.once('close', () => { rm(file, { force: true }).catch(() => {}); });
    try {
      await pipeline(source, res);
    } catch {
      // The panel hung up part way. Nothing to answer; the copy still goes.
    }
  });

  app.use('/ops', router);
  console.log('[ops] operations API on /ops, for loopback callers with the token');
  return true;
}

function allowed(req, token) {
  if (!LOOPBACK.has(req.socket.remoteAddress)) return false;
  // Caddy and a tunnel connector both connect from this machine, and both
  // say so in one of these. Their presence at all is the refusal.
  if (req.get('x-forwarded-for') !== undefined || req.get('forwarded') !== undefined) return false;
  const header = req.get('authorization') ?? '';
  return header.startsWith('Bearer ') && secretsMatch(header.slice(7), token);
}

/** One account as the panel sees it: the banned included, and whether they are on. */
function opsAccount(u, online) {
  return {
    id: u.id,
    nickname: u.nickname,
    displayName: u.display_name || u.nickname,
    role: u.role,
    createdAt: u.created_at,
    online: online.has(u.id),
    banned: u.banned_at != null,
    bannedAt: u.banned_at ?? null,
  };
}

/** The database's size on disk, the WAL included: it is where the newest writes are. */
function databaseBytes(path) {
  let total = 0;
  for (const file of [path, `${path}-wal`]) {
    try { total += statSync(file).size; } catch { /* no WAL right now */ }
  }
  return total;
}

/** Snapshots a crash left half-sent. They are nobody's, and each is a whole database. */
function sweepBackups(dataDir) {
  try {
    for (const name of readdirSync(dataDir)) {
      if (name.startsWith(BACKUP_PREFIX)) rmSync(join(dataDir, name), { force: true });
    }
  } catch { /* nothing to sweep */ }
}
