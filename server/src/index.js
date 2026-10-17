// Harmony control server.
//
// Two jobs, nothing more:
//   * /api/*          -- the client asks "is this username free?" and gets back
//                        either a publish token or a watch URL.
//   * /mediamtx/auth  -- MediaMTX asks "may this connection publish/read?".
//
// No media touches this process.

import { join } from 'node:path';
import express from 'express';
import { config, iceServers, signalingBase, whepUrl, whipUrl } from './config.js';
import { LoginLimiter, newMediaToken, secretsMatch } from './auth.js';
import { Throttle } from './throttle.js';
import { MediaMtxMonitor } from './mediamtx-api.js';
import { Rooms, normalizeUsername, normalizePath } from './rooms.js';
import { ensureDataDir, openDatabase, meta } from './db.js';
import { ServerSettings } from './server-settings.js';
import {
  Accounts, claimOwner, ensureOwnerToken, normalizeNickname, publicUser, sessionHash,
} from './accounts.js';
import {
  Channels,
  VoiceRooms,
  callPath,
  channelPath,
  channelSecret,
  mintChannelToken,
  parseCallPath,
  parseChannelPath,
  publicChannel,
  publicGroup,
  readChannelToken,
  MEDIA_KINDS,
  TOKEN_FLAGS,
} from './channels.js';
import { Realtime } from './realtime.js';
import { claimedMediaKeyAccount, mintMediaKey, verifyMediaKey } from './media-keys.js';
import { DirectMessages, publicConversation, publicDmMessage } from './dms.js';
import { mountDirectMessages } from './dm-routes.js';
import { mountOps } from './ops.js';
import { Calls } from './calls.js';
import {
  Chat, Soundpad, Emojis,
  publicMessage, publicReply, publicClip, publicEmoji, allowedTypes, mediaTypeOf, mentionsIn,
  MAX_UPLOAD_BYTES, MAX_AVATAR_BYTES, MAX_CLIP_BYTES, MAX_EMOJI_BYTES, MAX_EMOJIS,
  MAX_FILE_LIMIT, MIN_FILE_LIMIT, DEFAULT_FILE_BYTES,
} from './chat.js';
import { CHUNK_BYTES, UploadSessions } from './upload-sessions.js';

const app = express();
app.disable('x-powered-by');

/**
 * Trust only a proxy on this machine -- Caddy, or a Cloudflare Tunnel connector.
 *
 * `true` would trust the whole X-Forwarded-For chain, and every entry in that
 * header except the one our own proxy appends is written by the client. With a
 * password to guess that is not academic: the rate limiter is keyed on the
 * address, so a spoofable address is a rate limiter that can be stepped around
 * by changing one header. 'loopback' makes Express walk back only through
 * proxies it actually trusts, landing on the address Caddy observed.
 */
app.set('trust proxy', 'loopback');

/*
 * Request bodies are small JSON everywhere except a private conversation's
 * routes, where a message arrives sealed: a 4000-character body that is all
 * emoji is 16 KB of UTF-8 before it is encrypted and base64'd. Those get a
 * bigger allowance -- still a ceiling, enforced again per field in dms.js --
 * and everything else keeps the tight one.
 */
const smallJson = express.json({ limit: '16kb' });
const sealedJson = express.json({ limit: '96kb' });
app.use((req, res, next) => (req.path.startsWith('/api/dm') ? sealedJson : smallJson)(req, res, next));

const db = openDatabase({ dataDir: ensureDataDir(config.dataDir) });
const metaStore = meta(db);
const accounts = new Accounts(db);
ensureOwnerToken(accounts, metaStore);

const channels = new Channels(db);
/*
 * A locked channel silences everyone but the owner. Admins included: the
 * lock is the owner's stage, and an admin who needs to speak can ask for it
 * to be lifted -- an exemption by role would quietly turn "only the owner"
 * into "only staff".
 */
const voice = new VoiceRooms({
  micLocked: (channelId, userId) => Boolean(channels.get(channelId)?.mic_locked)
    && accounts.byId(userId)?.role !== 'owner',
});
const mediaSecret = channelSecret(metaStore);
const chat = new Chat(db, {
  dataDir: config.dataDir, maxDiskBytes: config.maxDiskBytes, diskReserveBytes: config.diskReserveBytes,
});
const uploads = new UploadSessions(chat);
const soundpad = new Soundpad(db);
const emojis = new Emojis(db);
const dms = new DirectMessages(db);
/*
 * Calls in private conversations. `record` writes the call's line into the
 * conversation -- "missed call", "call, 5 minutes" -- and tells both people,
 * exactly as a message they sent would.
 */
const calls = new Calls({
  graceMs: config.callGraceMs,
  notify: (userIds, payload) => { for (const id of userIds) realtime?.toUser(id, payload); },
  record: (conversationId, callerId, meta) => {
    const conversation = dms.get(conversationId);
    if (!conversation) return;
    const message = publicDmMessage(dms.record(conversationId, callerId, meta));
    for (const id of DirectMessages.members(conversation)) {
      realtime?.toUser(id, {
        type: 'dm:message', message, conversation: publicConversation(dms.get(conversationId)),
      });
    }
  },
});
/*
 * The name and the door key.
 *
 * The environment is still the default; this only looks at server_meta once
 * the owner has set something there. config.password is read exactly once
 * more, to seed it, and nothing else in this file should consult it again.
 */
const serverSettings = new ServerSettings(metaStore, { envPassword: config.password });
chat.maxFileBytes = serverSettings.maxFileBytes ?? DEFAULT_FILE_BYTES;

const rooms = new Rooms({ claimTtlMs: config.claimTtlMs });
const limiter = new LoginLimiter({
  maxAttempts: config.maxLoginAttempts,
  lockoutMinutes: config.lockoutMinutes,
});

/**
 * A second limiter for account logins, keyed by nickname rather than address.
 *
 * The shared-password limiter is keyed by IP, which is the right control for
 * "someone is guessing the door key". It is the wrong one here: everybody
 * behind one NAT shares an address, so one person fat-fingering their password
 * would lock out the household. Keying by nickname also means an attacker who
 * rotates addresses still cannot grind a single account.
 */
const loginLimiter = new LoginLimiter({
  maxAttempts: config.maxLoginAttempts,
  lockoutMinutes: config.lockoutMinutes,
});
const monitor = new MediaMtxMonitor({
  apiUrl: config.mediamtxApi,
  intervalMs: config.pollIntervalMs,
});

monitor.on('paths', (paths) => rooms.syncFromPaths(paths));
monitor.on('down', (err) => console.warn(`[mediamtx] control API unreachable: ${err.message}`));
monitor.on('up', () => console.log('[mediamtx] control API connected'));

/*
 * The host's admin panel: see ops.js. Ahead of the CORS headers below, so
 * that a web page open in a browser on this machine is not invited to call
 * it, and ahead of the door, which is for members.
 */
mountOps(app, {
  token: config.opsToken,
  build: config.build,
  db,
  dataDir: config.dataDir,
  dbPath: join(config.dataDir, 'harmony.db'),
  accounts,
  metaStore,
  serverSettings,
  chat,
  channels,
  voice,
  calls,
  loginLimiter,
  realtime: () => realtime,
  serverView: () => serverView(),
  removeAccount,
  /*
   * The door, changed by the host: config.password follows, and so does the
   * watch token, which is minted at boot only on a passworded server. Kept
   * in step here, the legacy watch URLs follow the door at once rather than
   * after a restart, and restartRequired stays true to what it measures.
   */
  setDoorPassword: (password) => {
    serverSettings.useEnvironmentPassword(password);
    config.password = password;
    config.mediaToken = password ? (config.mediaToken || newMediaToken()) : '';
  },
});

// The desktop client is not served from a browser origin, but allowing CORS
// keeps a plain browser usable for the read-only stream list.
app.use((req, res, next) => {
  res.set('Access-Control-Allow-Origin', '*');
  res.set('Access-Control-Allow-Headers', 'Content-Type, X-Harmony-Password, Authorization');
  res.set('Access-Control-Allow-Methods', 'GET,POST,OPTIONS');
  if (req.method === 'OPTIONS') return res.sendStatus(204);
  next();
});

/**
 * Gate for everything except /api/health.
 *
 * On an open server this is a no-op, so the un-passworded deployment keeps
 * exactly the behaviour it had. The password may arrive as a header or in the
 * JSON body; the header is what the client uses, the body form is there so the
 * endpoint can be exercised with curl.
 */
function requirePassword(req, res, next) {
  if (!serverSettings.passwordRequired) return next();

  const key = req.ip ?? 'unknown';
  const gate = limiter.check(key);
  if (!gate.allowed) {
    res.set('Retry-After', String(gate.retryAfterSec));
    return res.status(429).json({
      error: 'locked_out',
      retryAfterSec: gate.retryAfterSec,
      message: `Too many wrong passwords. Try again in ${formatWait(gate.retryAfterSec)}.`,
    });
  }

  const offered = req.get('x-harmony-password') ?? req.body?.password;

  // Offering nothing is not a guess, and must not burn an attempt. The client
  // asks for the stream list as soon as it opens, before the user has typed
  // anything -- counting that would let someone lock themselves out of their
  // own server in three refreshes without ever getting a password wrong.
  if (!offered) {
    return res.status(401).json({
      error: 'password_required',
      message: 'This server needs a password.',
    });
  }

  if (serverSettings.matches(offered)) {
    limiter.succeed(key);
    return next();
  }

  const result = limiter.fail(key);
  console.warn(
    `[auth] wrong password from ${key}` +
      (result.locked ? ` -- locked out for ${formatWait(result.retryAfterSec)}` : ''),
  );
  if (result.locked) {
    res.set('Retry-After', String(result.retryAfterSec));
    return res.status(429).json({
      error: 'locked_out',
      retryAfterSec: result.retryAfterSec,
      message: `Too many wrong passwords. Try again in ${formatWait(result.retryAfterSec)}.`,
    });
  }
  return res.status(401).json({
    error: 'bad_password',
    attemptsLeft: result.attemptsLeft,
    message: `Wrong password. ${result.attemptsLeft} ${result.attemptsLeft === 1 ? 'try' : 'tries'} left before a lockout.`,
  });
}

function formatWait(seconds) {
  if (seconds < 90) return `${seconds} seconds`;
  return `${Math.ceil(seconds / 60)} minutes`;
}

// ---------------------------------------------------------------------------
// Client API
// ---------------------------------------------------------------------------

/**
 * Deliberately outside the password gate: the client has to be able to ask
 * "does this server want a password?" before it can sensibly prompt for one.
 * It gives away nothing but that answer until the caller authenticates.
 */
app.get('/api/health', (req, res) => {
  // Checking the password here used to be free: no rate limiting, so
  // `authenticated` flipped to true on a correct guess without ever burning an
  // attempt -- an unmetered oracle sitting next to a metered one. Offering
  // nothing is still free, because the client probes this endpoint before the
  // user has typed anything.
  let authed = !serverSettings.passwordRequired;
  if (serverSettings.passwordRequired) {
    const offered = req.get('x-harmony-password');
    const key = req.ip ?? 'unknown';
    const gate = limiter.check(key);
    if (offered && !gate.allowed) {
      res.set('Retry-After', String(gate.retryAfterSec));
      return res.status(429).json({
        error: 'locked_out',
        retryAfterSec: gate.retryAfterSec,
        message: `Too many wrong passwords. Try again in ${formatWait(gate.retryAfterSec)}.`,
      });
    }
    if (offered) {
      authed = serverSettings.matches(offered);
      if (authed) limiter.succeed(key);
      else limiter.fail(key);
    }
  }
  res.json({
    ok: monitor.reachable,
    mediamtx: monitor.reachable ? 'up' : 'down',
    passwordRequired: serverSettings.passwordRequired,
    authenticated: authed,
    // `inbox`: GET /api/inbox and `scope: 'private'` sockets, so a client can
    // keep this server's private messages in the background cheaply.
    features: ['inbox', 'replies', 'edit-history', 'ink', 'dm-clear'],
    ...(authed
      ? {
          name: serverSettings.name,
          // The picture's hash, downloadable from /api/server/icon/:hash (or
          // /api/server/logo) without an account. Under the Electron line's
          // name only: native clients from 4.0.8 read `logo` as `iconHash` and
          // refuse a reply that carries both. See publicView.
          logo: serverSettings.iconHash,
          signalingBase: signalingBase(req.headers.host),
          liveStreams: rooms.listLive().length,
          // Lets the client show "create the first account" rather than a
          // login form on a brand new server.
          hasAccounts: !accounts.isEmpty,
          needsOwner: !accounts.hasOwner,
        }
      : {}),
  });
});

/*
 * Media keys are signed with the account's password hash folded in, so a key
 * dies with the sessions it was issued beside: a password change ends every
 * session and, through this, every media link -- and so does removing the
 * account, whose id SQLite may hand to the next person to register (users.id
 * is a plain INTEGER PRIMARY KEY, and the highest one is reused once
 * deleted). A new hash has a new salt, so an old key cannot verify against it.
 */
const mediaKeySecret = (user) => `${mediaSecret}:${user.password_hash}`;

/**
 * A stored file, by media key rather than by headers.
 *
 * For clients that load uploads straight into <img>/<video>/<audio> -- the
 * browser and Android builds -- which cannot attach the door password or a
 * session token to those requests. The key stands in for both: it is only
 * issued behind the door and a login (GET /api/media-key), and it names the
 * account, which must still exist. Registered BEFORE the door on purpose;
 * the key is what was checked at the door. See media-keys.js.
 *
 * It opens what /api/uploads opens -- sendUpload is shared -- so a private
 * conversation's sealed file is not here either.
 */
app.get('/api/media/:hash', (req, res) => {
  const user = accounts.byId(claimedMediaKeyAccount(req.query.k));
  if (!user || user.banned_at != null || verifyMediaKey(mediaKeySecret(user), req.query.k) !== user.id) {
    return res.status(401).json({ error: 'bad_media_key', message: 'That media link has expired.' });
  }
  return sendUpload(req, res, { scope: 'private' });
});

app.use('/api', requirePassword);

// ---------------------------------------------------------------------------
// Accounts
//
// These sit BEHIND the shared password: it is still the door, accounts are the
// rooms. A stranger cannot reach any of this, let alone enumerate nicknames.
// ---------------------------------------------------------------------------

/**
 * Resolve `Authorization: Bearer <token>` to req.user, or leave it undefined.
 *
 * Never rejects on its own -- plenty of routes are usable logged out, and the
 * ones that are not say so themselves. That keeps "who may do this" next to the
 * thing being done rather than in a middleware chain.
 */
app.use('/api', (req, _res, next) => {
  const header = req.get('authorization') ?? '';
  const token = header.startsWith('Bearer ') ? header.slice(7) : null;
  if (token) {
    const user = accounts.resolveSession(token);
    if (user) {
      req.user = user;
      req.sessionToken = token;
    }
  }
  next();
});

const requireLogin = (req, res, next) => {
  if (!req.user) {
    return res.status(401).json({ error: 'login_required', message: 'Log in first.' });
  }
  return next();
};

const requireRole = (...roles) => (req, res, next) => {
  if (!req.user) {
    return res.status(401).json({ error: 'login_required', message: 'Log in first.' });
  }
  if (!roles.includes(req.user.role)) {
    return res.status(403).json({ error: 'forbidden', message: 'You do not have permission.' });
  }
  return next();
};

app.post('/api/accounts/register', async (req, res) => {
  const result = await accounts.register(req.body?.nickname, req.body?.password);
  if (!result.ok) {
    const messages = {
      invalid_nickname:
        '2-20 characters: letters, digits, hyphen or underscore, starting with a letter or '
        + 'digit. Cannot start with "vc-" or end with "-cam".',
      weak_password: 'Use at least 6 characters.',
      nickname_taken: 'That nickname is already registered.',
    };
    return res.status(400).json({ error: result.error, message: messages[result.error] });
  }

  // The owner key is offered at registration so claiming ownership is one step
  // rather than "register, then find the other box".
  const becameOwner = req.body?.ownerKey
    ? claimOwner(accounts, metaStore, result.user.id, req.body.ownerKey)
    : false;

  const user = accounts.find(result.user.nickname);
  const token = accounts.startSession(user.id);
  /*
   * Everybody else learns there is somebody new.
   *
   * The member list is drawn from the accounts each client already holds,
   * crossed with who is online -- so a new account appeared in nobody's
   * list until they restarted, even once its owner was online. user:updated
   * is what every client already handles by adding to that map; a new
   * account is just one nobody had yet.
   */
  realtime?.broadcast({ type: 'user:updated', user: publicUser(user) });
  return res.status(201).json({
    user: publicUser(user),
    token,
    ownerClaimed: becameOwner,
    // Tell them the key was wrong rather than silently making them a member.
    ownerKeyRejected: Boolean(req.body?.ownerKey) && !becameOwner,
  });
});

app.post('/api/accounts/login', async (req, res) => {
  // The account's own name is the key, as `verify` finds it: keyed on the text
  // as typed, "b ob" and "bo b" would each get fresh tries at bob. Names that
  // can't be an account share one key, so they can't grow the limiter either.
  const nickname = normalizeNickname(req.body?.nickname) ?? '';
  const gate = loginLimiter.check(nickname);
  if (!gate.allowed) {
    res.set('Retry-After', String(gate.retryAfterSec));
    return res.status(429).json({
      error: 'locked_out',
      retryAfterSec: gate.retryAfterSec,
      message: `Too many wrong passwords for "${nickname}". Try again in ${formatWait(gate.retryAfterSec)}.`,
    });
  }

  const user = await accounts.verify(nickname, req.body?.password);
  if (!user) {
    const result = loginLimiter.fail(nickname);
    if (result.locked) {
      res.set('Retry-After', String(result.retryAfterSec));
      return res.status(429).json({
        error: 'locked_out',
        retryAfterSec: result.retryAfterSec,
        message: `Too many wrong passwords. Try again in ${formatWait(result.retryAfterSec)}.`,
      });
    }
    // One message for "no such account" and "wrong password" -- the pair would
    // otherwise tell an attacker which nicknames exist.
    return res.status(401).json({
      error: 'bad_credentials',
      attemptsLeft: result.attemptsLeft,
      message: 'Wrong nickname or password.',
    });
  }
  loginLimiter.succeed(nickname);
  // Only after the right password, so this cannot be used to find out who
  // is banned.
  if (user.banned_at != null) {
    return res.status(403).json({ error: 'banned', message: 'This account is banned from this server.' });
  }

  const becameOwner = req.body?.ownerKey
    ? claimOwner(accounts, metaStore, user.id, req.body.ownerKey)
    : false;

  const token = accounts.startSession(user.id);
  return res.json({
    user: publicUser(accounts.find(user.nickname)),
    token,
    ownerClaimed: becameOwner,
    ownerKeyRejected: Boolean(req.body?.ownerKey) && !becameOwner,
  });
});

app.post('/api/accounts/logout', (req, res) => {
  if (req.sessionToken) {
    accounts.endSession(req.sessionToken);
    // Its sockets too: they authenticated once, at hello, and would go on
    // receiving private messages for a token that no longer exists.
    realtime?.closeSession(sessionHash(req.sessionToken));
  }
  res.json({ ok: true });
});

app.get('/api/accounts/me', requireLogin, (req, res) => {
  res.json({ user: publicUser(req.user) });
});

app.post('/api/accounts/password', requireLogin, async (req, res) => {
  const ok = await accounts.verify(req.user.nickname, req.body?.current);
  if (!ok) {
    return res.status(401).json({ error: 'bad_credentials', message: 'Current password is wrong.' });
  }
  const result = await accounts.changePassword(req.user.id, req.body?.password);
  if (!result.ok) {
    return res.status(400).json({ error: result.error, message: 'Use at least 6 characters.' });
  }
  // changePassword drops every session for this account, including this one.
  // Every socket goes with them -- a stolen session's above all -- except the
  // ones this session opened, which carry on under the session issued here.
  const token = accounts.startSession(req.user.id);
  realtime?.closeOtherSessions(req.user.id, {
    keep: sessionHash(req.sessionToken), renamed: sessionHash(token),
  });
  return res.json({ ok: true, token });
});

/**
 * Set or clear your profile picture.
 *
 * Two steps on purpose: the picture is uploaded through /api/uploads like any
 * other file, and this only points the account at it. That means avatars
 * inherit the content-type allowlist, the disk quota and the content-addressed
 * storage for free, and two people who pick the same picture cost one copy.
 *
 * The reference juggling is the part that matters. An avatar nobody references
 * is an orphan the moment the next upload needs room, so the new file is
 * retained BEFORE the old one is released -- re-setting the same picture must
 * not momentarily drop it to zero. And the old one is ALWAYS released, the
 * same picture included: that took a second reference a moment ago, so the
 * count comes out at one, where skipping it gained one per re-set and the file
 * could never be evicted.
 */
app.post('/api/accounts/avatar', requireLogin, (req, res) => {
  const hash = req.body?.hash == null ? null : String(req.body.hash);

  if (hash !== null) {
    if (!/^[0-9a-f]{64}$/.test(hash)) {
      return res.status(400).json({ error: 'bad_hash', message: 'That is not an uploaded file.' });
    }
    const upload = chat.fileInfo(hash);
    if (!upload) {
      return res.status(400).json({ error: 'no_such_upload', message: 'Upload the picture first.' });
    }
    if (mediaTypeOf(upload.content_type) !== 'image') {
      return res.status(400).json({
        error: 'not_an_image',
        message: 'A profile picture has to be an image.',
      });
    }
    if (upload.bytes > MAX_AVATAR_BYTES) {
      return res.status(400).json({
        error: 'avatar_too_large',
        message: `Profile pictures are limited to ${Math.round(MAX_AVATAR_BYTES / 1024)} KB.`,
      });
    }
    chat.retain(hash);
  }

  const result = accounts.setAvatar(req.user.id, hash);
  if (!result.ok) {
    if (hash !== null) chat.release(hash);
    return res.status(404).json({ error: result.error });
  }
  if (result.previous) chat.release(result.previous);

  const user = publicUser(result.user);
  // Everyone draws everyone else's picture, so everyone needs to know.
  realtime?.broadcast({ type: 'user:updated', user });
  return res.json({ user });
});

/**
 * Change your own display name.
 *
 * Your OWN: there is no id in the path. Renaming other people is not a
 * thing an admin needs to do, and leaving it out means there is no
 * permission check here to get wrong.
 */
app.post('/api/accounts/display-name', requireLogin, (req, res) => {
  const result = accounts.setDisplayName(req.user.id, req.body?.displayName);
  if (!result.ok) {
    const messages = {
      no_such_user: 'No such user.',
      too_long: 'That name is too long. 32 characters at most.',
      bad_characters: 'That name contains characters that cannot be displayed.',
    };
    return res.status(400).json({ error: result.error, message: messages[result.error] });
  }
  const user = publicUser(result.user);
  realtime?.broadcast({ type: 'user:updated', user });
  return res.json({ user });
});

/** Anyone logged in may see the roster; it is a friends' server, not a forum. */
app.get('/api/accounts', requireLogin, (_req, res) => {
  res.json({ users: accounts.list().map((u) => publicUser(u)) });
});

/**
 * Change somebody's role.
 *
 * An admin may hand out admin, and that is all. Only the owner may take it
 * away, and only the owner may make another owner.
 *
 * The asymmetry is deliberate and is the usual shape for this: trusting
 * someone enough to let them in is a smaller decision than being able to
 * throw out the person who trusted you. With symmetric powers any two
 * admins can demote each other, and the first one to click wins -- which
 * turns a falling-out between friends into an irreversible race.
 *
 * Enforced here rather than by hiding the control, because the control
 * being hidden is a statement about one client's UI and this is a statement
 * about the server.
 */
app.post('/api/accounts/:id/role', requireRole('owner', 'admin'), (req, res) => {
  const id = Number.parseInt(req.params.id, 10);
  const role = String(req.body?.role ?? '');

  if (req.user.role !== 'owner') {
    const target = accounts.byId(id);
    if (!target) {
      return res.status(400).json({ error: 'no_such_user', message: 'No such user.' });
    }
    // Promotion only, to admin only, and only from member -- which also
    // stops an admin demoting themselves into a server with no admins.
    if (role !== 'admin' || target.role !== 'member') {
      return res.status(403).json({
        error: 'owner_only',
        message: 'Only the owner can remove an admin or appoint another owner.',
      });
    }
  }

  const result = accounts.setRole(id, role);
  if (!result.ok) {
    const messages = {
      invalid_role: 'Role must be owner, admin or member.',
      no_such_user: 'No such user.',
      last_owner: 'There has to be at least one owner.',
    };
    return res.status(400).json({ error: result.error, message: messages[result.error] });
  }
  const user = publicUser(result.user);
  realtime?.broadcast({ type: 'user:updated', user });
  return res.json({ user });
});

/**
 * Everything needed to watch, without reserving anything.
 *
 * The mosaic opens several streams at once, and going through /api/session for
 * each would be a trap: a name that stops being live between the listing and
 * the call comes back as "free", and the viewer would silently claim someone
 * else's username. Watching is public, so the watch URL belongs here.
 */
/**
 * Delete an account, and everything written under it.
 *
 * OWNER ONLY, and not even an admin. Granting admin is reversible and
 * force-muting somebody lasts until they are unmuted; this takes a person's
 * whole history off the server and there is no undo anywhere in the app.
 * The same reasoning already makes removing an admin owner-only.
 *
 * The owner cannot be deleted at all, including by themselves: a server
 * with no owner has no way back short of editing the database by hand.
 */
app.post('/api/accounts/:id/delete', requireRole('owner'), (req, res) => (
  removeAccount(res, Number.parseInt(req.params.id, 10), req.user.nickname)
));

/** The whole of deleting an account, answered on `res`: see the route above. Shared with ops.js. */
function removeAccount(res, id, by) {
  const target = accounts.byId(id);
  if (!target) return res.status(404).json({ error: 'no_such_user' });

  const result = accounts.remove(id);
  if (!result.ok) {
    return res.status(result.error === 'cannot_remove_owner' ? 403 : 404).json({
      error: result.error,
      message: result.error === 'cannot_remove_owner'
        ? 'The owner cannot be removed. Hand ownership over first.'
        : 'No such account.',
    });
  }

  // Their sessions went with the row, so every later request fails -- but a
  // socket authenticates once and would go on receiving everything. A call
  // they were in ends too; its conversation went with them, so it leaves no line.
  calls.leaveUser(id);
  realtime?.kickUser(id);
  // Everything that named them is now wrong: the member list, every message
  // they wrote, every roster they were in.
  realtime?.broadcast({ type: 'accounts', users: accounts.list().map(publicUser) });
  realtime?.broadcastPresence();
  console.warn(`[accounts] ${target.nickname} removed by ${by}, `
    + `${result.messages} messages deleted`);
  return res.json({ ok: true, messages: result.messages });
}

// ---------------------------------------------------------------------------
// The server itself
// ---------------------------------------------------------------------------

/*
 * What the server says about itself, with the upload limit beside the name:
 * every client checks a file against maxFileBytes before sending it, and the
 * owner's settings offer up to maxFileLimit. A client that sees maxFileBytes
 * also knows it may send files in pieces (upload-sessions.js).
 */
const serverView = () => ({
  ...serverSettings.publicView(),
  maxFileBytes: chat.maxFileBytes,
  maxFileLimit: MAX_FILE_LIMIT,
});

app.get('/api/server', requireLogin, (_req, res) => {
  res.json({ server: serverView() });
});

/**
 * Why a hash cannot be the server's picture, or null when it can. The rules
 * are an avatar's, size cap included: it is drawn as small, as often.
 */
function iconProblem(hash) {
  if (!/^[0-9a-f]{64}$/.test(hash)) {
    return { error: 'bad_hash', message: 'That is not an uploaded file.' };
  }
  const upload = chat.fileInfo(hash);
  if (!upload) return { error: 'no_such_upload', message: 'Upload the picture first.' };
  if (mediaTypeOf(upload.content_type) !== 'image') {
    return { error: 'not_an_image', message: 'A picture has to be an image.' };
  }
  if (upload.bytes > MAX_AVATAR_BYTES) {
    return {
      error: 'picture_too_large',
      message: `Pictures are limited to ${Math.round(MAX_AVATAR_BYTES / 1024)} KB.`,
    };
  }
  return null;
}

/**
 * Rename the server, change the key to its front door, or set its picture.
 *
 * Owner only. `password` is applied only when the field is present at all,
 * so renaming cannot clear the password by omission -- and an empty string
 * IS a value here, meaning "take the door off", which is why the two cases
 * have to be told apart rather than both treated as falsy. `iconHash` works
 * the same way: absent leaves the picture, null removes it. `logo` is the
 * same field under the name the other clients send. The native client sends
 * both, to work against either line's server, so they are ONE field: with
 * `iconHash` present `logo` is not looked at, and the upload is retained
 * and released once.
 *
 * The picture is checked before anything is applied, so a bad hash does not
 * leave a half-made change behind. Its references are juggled as an avatar's
 * are: the new one retained before the old one is released.
 *
 * `maxFileBytes` is the largest file anybody may send, null for the default.
 * It is not checked against the disk: the disk changes, and the floor in
 * Chat#room is what keeps the server running when it fills.
 */
app.post('/api/server', requireRole('owner'), (req, res) => {
  const icon = req.body?.iconHash !== undefined ? req.body.iconHash : req.body?.logo;
  if (icon !== undefined && icon !== null) {
    const problem = iconProblem(String(icon));
    if (problem) return res.status(400).json(problem);
  }
  const rawLimit = req.body?.maxFileBytes;
  const limit = rawLimit === undefined || rawLimit === null ? rawLimit : Math.round(Number(rawLimit));
  if (typeof limit === 'number' && !(limit >= MIN_FILE_LIMIT && limit <= MAX_FILE_LIMIT)) {
    return res.status(400).json({
      error: 'bad_limit',
      message: `The largest file has to be between 1 MB and ${MAX_FILE_LIMIT / 1024 ** 3} GB.`,
    });
  }

  if (req.body?.name !== undefined) serverSettings.setName(req.body.name);
  if (typeof req.body?.password === 'string') serverSettings.setPassword(req.body.password);
  if (icon !== undefined) {
    const hash = icon === null ? null : String(icon);
    if (hash) chat.retain(hash);
    // Always the old one's reference back: re-setting the same picture took a
    // second one a moment ago, so the count still comes out at one.
    const previous = serverSettings.setIcon(hash);
    if (previous) chat.release(previous);
  }
  if (limit !== undefined) {
    serverSettings.setMaxFileBytes(limit);
    chat.maxFileBytes = serverSettings.maxFileBytes ?? DEFAULT_FILE_BYTES;
  }

  const view = serverView();
  realtime?.broadcast({ type: 'server', server: view });
  return res.json({ server: view });
});

/**
 * The server's picture, to anyone through the door, signed in or not.
 *
 * The sign-in screen and the server rail draw it before there is an account
 * to download with, which is why this is not /api/uploads. Only the hash the
 * server currently shows is served, so this cannot be used to read any other
 * upload without signing in. The door password still applies (this sits
 * behind requirePassword), exactly as the server's name does in /api/health.
 */
app.get('/api/server/icon/:hash', (req, res) => {
  const hash = String(req.params.hash);
  if (!hash || hash !== serverSettings.iconHash) return res.status(404).json({ error: 'no_such_file' });
  return sendServerIcon(res, hash, { error: 'no_such_file' });
});

/**
 * The same picture with no hash in the path: whatever it is now, which is
 * how the Electron, browser and Android clients ask for it. X-Harmony-Hash
 * says which one it was, for a client that caches by hash.
 *
 * Not cached as immutable, unlike the hashed path: this URL stays put when
 * the picture changes, and a browser holding last year's copy for a year
 * would never see the new one. Revalidating costs a 304.
 */
app.get('/api/server/logo', (_req, res) => {
  const hash = serverSettings.iconHash;
  if (!hash) return res.status(404).json({ error: 'no_logo' });
  res.set('X-Harmony-Hash', hash);
  return sendServerIcon(res, hash, { error: 'no_logo' }, 'no-cache');
});

function sendServerIcon(res, hash, notFound, cache = 'public, max-age=31536000, immutable') {
  const info = chat.fileInfo(hash);
  if (!info) return res.status(404).json(notFound);
  res.set('Content-Type', info.content_type);
  res.set('X-Content-Type-Options', 'nosniff');
  res.set('Content-Security-Policy', "default-src 'none'; sandbox");
  res.set('Cache-Control', cache);
  return res.sendFile(info.path, STORED_FILE);
}

/*
 * res.sendFile's options for a stored file. The path is built from a checked
 * hash, never from the request, so a dot anywhere in it is the data
 * directory's -- ~/.ciranda, say. Express 5 refuses ("ignore": a 404) a path
 * with a dot-folder anywhere in it unless told otherwise.
 */
const STORED_FILE = { dotfiles: 'allow' };

/**
 * How much of the upload quota is used. The upload replies carry this too,
 * but somebody looking after the server should not have to upload a file to
 * find out. One SUM over the uploads table: cheap enough to ask for freely.
 */
app.get('/api/server/storage', requireRole('owner', 'admin'), (_req, res) => {
  res.json({ usedBytes: chat.usedBytes, quotaBytes: chat.quotaBytes });
});

app.get('/api/streams', (req, res) => {
  const base = signalingBase(req.headers.host);
  res.json({
    streams: rooms.listLive().map((stream) => ({ ...stream, whepUrl: whepUrl(stream.username, base) })),
    iceServers: iceServers(),
  });
});

// The single entry point behind the username box. Always answers with a role.
app.post('/api/session', (req, res) => {
  /**
   * Who is allowed to claim what.
   *
   * This endpoint used to let anyone past the door password claim any free
   * name. Once nicknames are permanent identities that is impersonation: log in
   * as `bob`, claim the stream name `alice`, and your screen share appears
   * under her name to everyone watching. So an authenticated caller streams
   * under their own nickname and the body's `username` is ignored outright.
   *
   * The anonymous path survives only while no account exists, so an existing
   * 1.0.0 deployment keeps working right up until someone registers. After
   * that, logging in is required -- leaving it open would leave the hole open.
   */
  /*
   * `kind: 'camera'` claims the caller's OWN derived webcam path.
   *
   * It has to be a flag rather than a username, for the same reason the body's
   * username is ignored above: letting the client name the path is exactly the
   * impersonation this endpoint was fixed to prevent. The `-cam` suffix is
   * appended here, server-side, from the authenticated nickname -- and
   * normalizeUsername refuses anything ending in `-cam` as INPUT, so the
   * namespace cannot be squatted.
   */
  const wantsCamera = req.body?.kind === 'camera';
  if (wantsCamera && !req.user) {
    return res.status(401).json({
      error: 'login_required',
      message: 'Sign in before starting your camera.',
    });
  }

  const username = req.user
    ? `${req.user.nickname}${wantsCamera ? '-cam' : ''}`
    : normalizeUsername(req.body?.username);

  if (!req.user && !accounts.isEmpty) {
    return res.status(401).json({
      error: 'login_required',
      message: 'This server has accounts. Log in to stream under your own name.',
    });
  }

  if (!username) {
    return res.status(400).json({
      error: 'invalid_username',
      message: '2-24 characters: letters, digits, hyphen or underscore, starting with a letter or digit.',
    });
  }

  if (!monitor.reachable) {
    return res.status(503).json({
      error: 'media_server_down',
      message: 'The media server is not responding. Try again in a moment.',
    });
  }

  const result = rooms.claim(username, { token: req.body?.token, ip: req.ip });
  const base = signalingBase(req.headers.host);

  if (result.role === 'broadcaster') {
    return res.json({
      role: 'broadcaster',
      username,
      token: result.token,
      whipUrl: whipUrl(username, result.token, base),
      iceServers: iceServers(),
      heartbeatMs: Math.floor(config.claimTtlMs / 3),
    });
  }

  return res.json({
    role: 'viewer',
    username,
    pending: result.pending,
    whepUrl: whepUrl(username, base),
    iceServers: iceServers(),
  });
});

app.post('/api/session/heartbeat', (req, res) => {
  const username = normalizeUsername(req.body?.username);
  const ok = username && rooms.heartbeat(username, req.body?.token);
  res.status(ok ? 200 : 404).json({ ok: Boolean(ok), live: username ? rooms.isLive(username) : false });
});

app.post('/api/session/release', (req, res) => {
  const username = normalizeUsername(req.body?.username);
  const ok = username && rooms.release(username, req.body?.token);
  res.json({ ok: Boolean(ok) });
});

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

const requireAdmin = (req, res, next) => {
  if (!req.user) return res.status(401).json({ error: 'login_required', message: 'Log in first.' });
  if (req.user.role !== 'owner' && req.user.role !== 'admin') {
    return res.status(403).json({ error: 'forbidden', message: 'Admins only.' });
  }
  return next();
};

app.get('/api/channels', requireLogin, (req, res) => {
  res.json({
    channels: channels.list().map((c) => ({
      ...publicChannel(c),
      // So the client can show "you will not be asked again" rather than a
      // padlock on a channel this person already unlocked.
      unlocked: !c.password_hash || channels.hasGrant(c.id, req.user.id),
    })),
    groups: channels.groups().map(publicGroup),
    occupancy: voice.occupancy(),
    // Derived from open sockets, never stored. Empty when the realtime
    // layer is not up, which is honest: with no sockets nobody is online.
    online: realtime?.onlineUserIds() ?? [],
    // Same payload the WebSocket hello carries, so the read-only fallback
    // path the client uses when the socket is down shows the same sidebar.
    rosters: voice.allRosters(),
  });
});

app.post('/api/channels', requireAdmin, async (req, res) => {
  const result = await channels.create({
    kind: req.body?.kind,
    name: req.body?.name,
    password: req.body?.password,
  });
  if (!result.ok) {
    const messages = {
      invalid_kind: 'A channel is either voice or text.',
      invalid_name: 'Give it a name of 1-32 characters.',
    };
    return res.status(400).json({ error: result.error, message: messages[result.error] });
  }
  realtime?.broadcastChannels();
  return res.status(201).json({ channel: publicChannel(result.channel) });
});

// Registered BEFORE /api/channels/:id on purpose. Express matches routes in
// registration order, so with these the other way round a POST to
// /api/channels/reorder is matched by /:id with id="reorder", parsed as NaN,
// and answered "no such channel" -- a 400 that looks like a validation bug and
// is really a routing one.
app.post('/api/channels/groups', requireAdmin, (req, res) => {
  const result = channels.createGroup(req.body?.name);
  if (!result.ok) {
    return res.status(400).json({ error: result.error, message: 'Give the group a name.' });
  }
  realtime?.broadcastChannels();
  return res.status(201).json({ group: publicGroup(result.group) });
});

app.post('/api/channels/groups/:id', requireAdmin, (req, res) => {
  const result = channels.renameGroup(Number.parseInt(req.params.id, 10), req.body?.name);
  if (!result.ok) {
    const messages = {
      no_such_group: 'No such group.',
      invalid_name: 'Give the group a name.',
    };
    return res.status(400).json({ error: result.error, message: messages[result.error] });
  }
  realtime?.broadcastChannels();
  return res.json({ group: publicGroup(result.group) });
});

/**
 * Delete a group. Its channels survive, ungrouped.
 *
 * The schema's ON DELETE SET NULL does that, and it is deliberate: a group
 * is a folder for the sidebar, not something that owns what is inside it.
 * Deleting it must not take a year of chat with it.
 */
app.post('/api/channels/groups/:id/delete', requireAdmin, (req, res) => {
  const result = channels.removeGroup(Number.parseInt(req.params.id, 10));
  if (!result.ok) {
    return res.status(400).json({ error: result.error, message: 'No such group.' });
  }
  realtime?.broadcastChannels();
  return res.json({ ok: true });
});

/** The whole sidebar after a drag: see Channels.arrange. */
app.post('/api/channels/arrange', requireAdmin, (req, res) => {
  const result = channels.arrange({
    groups: req.body?.groups,
    channels: req.body?.channels,
  });
  if (!result.ok) {
    const messages = {
      no_such_channel: 'That channel is gone -- refresh and try again.',
      no_such_group: 'That group is gone -- refresh and try again.',
    };
    return res.status(400).json({ error: result.error, message: messages[result.error] });
  }
  realtime?.broadcastChannels();
  return res.json({
    channels: result.channels.map(publicChannel),
    groups: result.groups.map(publicGroup),
  });
});

app.post('/api/channels/reorder', requireAdmin, (req, res) => {
  const result = channels.reorder(Array.isArray(req.body?.ids) ? req.body.ids : []);
  if (!result.ok) {
    return res.status(400).json({
      error: result.error,
      message: 'Send every channel id exactly once, in the order you want.',
    });
  }
  realtime?.broadcastChannels();
  return res.json({ channels: result.channels.map(publicChannel) });
});

app.post('/api/channels/:id', requireAdmin, async (req, res) => {
  const id = Number.parseInt(req.params.id, 10);
  const micLocked = req.body?.micLocked;
  // The rest of the channel is any admin's to edit; the lock is the owner's,
  // since it is the owner it leaves speaking.
  if (micLocked !== undefined && req.user.role !== 'owner') {
    return res.status(403).json({
      error: 'owner_only',
      message: 'Only the owner can lock the microphones in a channel.',
    });
  }
  const result = await channels.update(id, {
    ...(req.body?.name !== undefined ? { name: req.body.name } : {}),
    ...(req.body?.password !== undefined ? { password: req.body.password } : {}),
    ...(micLocked !== undefined ? { micLocked: Boolean(micLocked) } : {}),
  });
  if (!result.ok) return res.status(400).json({ error: result.error });
  realtime?.broadcastChannels();
  if (micLocked !== undefined) {
    // Same order as a force-mute, for the same reason: the roster tells the
    // people being silenced before the kick drops their session, or they
    // would read it as a network error. The auth hook is what keeps them
    // silent after that -- the kick only ends what is already in flight.
    realtime?.broadcastRoster(id);
    if (micLocked) {
      await Promise.all(voice.roster(id)
        .filter((m) => m.micLocked)
        .map((m) => monitor.kickPath(channelPath(id, m.mid, 'voice'))));
    }
  }
  return res.json({ channel: publicChannel(result.channel) });
});

app.post('/api/channels/:id/delete', requireAdmin, (req, res) => {
  const id = Number.parseInt(req.params.id, 10);
  const result = channels.remove(id);
  if (!result.ok) return res.status(404).json({ error: result.error });
  // Anyone sitting in it has nowhere to be any more. Left in the room, they'd
  // keep getting media tokens for it, and a new channel can be given the same
  // id: they'd be in that one, password or not.
  for (const { mid, userId } of voice.closeRoom(id)) {
    realtime?.toUser(userId, { type: 'voice:moved', channelId: null, by: req.user.nickname });
    monitor.kickPath(channelPath(id, mid, 'voice')).catch(() => {});
  }
  // An empty roster, so nobody is still shown in a channel that's gone.
  realtime?.broadcastRoster(id);
  realtime?.broadcastChannels();
  return res.json({ ok: true });
});

// ---------------------------------------------------------------------------
// Chat: messages, pins, search and uploads
// ---------------------------------------------------------------------------

/** A channel the caller is allowed to read, or null. */
function readableChannel(req, id) {
  const channel = channels.get(id);
  if (!channel) return null;
  if (channel.password_hash && !channels.hasGrant(id, req.user.id)) return null;
  return channel;
}

/**
 * publicMessage over a list, with its reactions and its mentions attached.
 *
 * Here rather than inside publicMessage because both need something it has
 * not got: the reactions are a second query, and the mentions need the
 * nickname table. Doing either per message would make a fifty-row page
 * fifty lookups.
 *
 * EVERY route that hands a client a message goes through this. A field
 * missing from one of them and present in another is the worst case: the
 * client draws it from the pushed copy and then loses it on the next
 * refresh, which looks like the feature working intermittently.
 */
function forClient(messages) {
  const list = [].concat(messages).filter(Boolean);
  const byId = chat.reactionsFor(list.map((m) => m.id));
  // Built once for the page. Eleven people on a friends' server, so the map
  // is cheaper than the eleven-row query it replaces per message.
  const ids = new Map(accounts.list().map((u) => [u.nickname, u.id]));
  const idOf = (nickname) => ids.get(nickname) ?? null;
  return list.map((m) => {
    const mentions = mentionsIn(m.body, idOf);
    // What a reply answers, as much as a reader needs to recognise it without
    // having that page loaded. Answering somebody tells them, as a mention would.
    const answered = m.reply_to ? chat.get(m.reply_to) : null;
    if (answered && answered.user_id !== m.user_id && !mentions.userIds.includes(answered.user_id)) {
      mentions.userIds.push(answered.user_id);
    }
    const reply = m.reply_to ? publicReply(m.reply_to, answered) : null;
    return publicMessage(m, byId.get(m.id) ?? [], mentions, reply);
  });
}

const oneForClient = (message) => forClient([message])[0] ?? null;

/**
 * Push something about a message to everyone who may read where it lives.
 *
 * An open channel is everybody, which is the plain broadcast. A password
 * channel is only the people holding a grant -- otherwise every message in
 * it reaches every connected client and is merely not drawn, which is not
 * the same thing as not being sent. It matters more now than it did: a
 * client that receives a message it cannot read would ring a mention bell
 * for a conversation it is not in.
 */
function toChannelReaders(channelId, payload) {
  const channel = channels.get(channelId);
  if (!channel?.password_hash) return realtime?.broadcast(payload);
  return realtime?.broadcastWhere(payload, (user) => channels.hasGrant(channelId, user.id));
}

app.get('/api/channels/:id/messages', requireLogin, (req, res) => {
  const id = Number.parseInt(req.params.id, 10);
  const channel = readableChannel(req, id);
  if (!channel) return res.status(404).json({ error: 'no_such_channel' });

  const before = Number.parseInt(req.query.before ?? '', 10);
  return res.json({
    messages: forClient(
      chat.history(id, { before: Number.isFinite(before) ? before : undefined }),
    ),
    pinned: forClient(chat.pinned(id)),
  });
});

app.post('/api/channels/:id/messages', requireLogin, (req, res) => {
  const id = Number.parseInt(req.params.id, 10);
  const channel = readableChannel(req, id);
  if (!channel) return res.status(404).json({ error: 'no_such_channel' });

  const result = chat.post({
    channelId: id,
    user: req.user,
    body: req.body?.body,
    attachmentHash: req.body?.attachmentHash ?? null,
    attachmentName: req.body?.attachmentName ?? null,
    replyTo: req.body?.replyTo ?? null,
  });
  if (!result.ok) return res.status(400).json({ error: result.error });

  const message = oneForClient(result.message);
  toChannelReaders(id, { type: 'message', message });
  return res.status(201).json({ message });
});

app.post('/api/messages/:id/pin', requireLogin, (req, res) => {
  const id = Number.parseInt(req.params.id, 10);
  // Pinning hands back the whole message: only for those who may read it.
  const existing = chat.get(id);
  if (!existing || !readableChannel(req, existing.channel_id)) {
    return res.status(404).json({ error: 'no_such_message' });
  }
  const result = chat.setPinned(id, req.body?.pinned !== false);
  if (!result.ok) return res.status(404).json({ error: result.error });
  const message = oneForClient(result.message);
  toChannelReaders(message.channelId, { type: 'message:updated', message });
  return res.json({ message });
});

/**
 * Change what a message says.
 *
 * YOUR OWN ONLY, and an admin is not an exception -- which is the one
 * place this app's admin rules are narrower than for deleting. Deleting
 * somebody's message removes it and everyone can see that it is gone;
 * editing it would put words in their mouth under their name, and nothing
 * in the UI could tell the difference.
 */
app.post('/api/messages/:id/edit', requireLogin, (req, res) => {
  const id = Number.parseInt(req.params.id, 10);
  const existing = chat.get(id);
  if (!existing) return res.status(404).json({ error: 'no_such_message' });
  if (!readableChannel(req, existing.channel_id)) {
    return res.status(404).json({ error: 'no_such_channel' });
  }
  if (existing.user_id !== req.user.id) {
    return res.status(403).json({
      error: 'forbidden',
      message: 'You can only edit your own messages.',
    });
  }

  const result = chat.edit(id, req.body?.body);
  if (!result.ok) {
    return res.status(400).json({
      error: result.error,
      message: result.error === 'empty_message'
        ? 'A message has to say something, unless it carries a file.'
        : 'No such message.',
    });
  }

  const message = oneForClient(result.message);
  toChannelReaders(existing.channel_id, { type: 'message:updated', message });
  return res.json({ message });
});

/** What a message said before each edit, for anybody who may read it. */
app.get('/api/messages/:id/edits', requireLogin, (req, res) => {
  const id = Number.parseInt(req.params.id, 10);
  const existing = chat.get(id);
  if (!existing || !readableChannel(req, existing.channel_id)) {
    return res.status(404).json({ error: 'no_such_message' });
  }
  return res.json({ versions: chat.versions(id) });
});

/**
 * Change the file on a message, or take it off.
 *
 * Your own only, the same rule editing follows and for the same reason:
 * swapping somebody's picture under their name is putting something in
 * their mouth, while deleting the message is visible to everyone.
 */
app.post('/api/messages/:id/attachment', requireLogin, (req, res) => {
  const id = Number.parseInt(req.params.id, 10);
  const existing = chat.get(id);
  if (!existing) return res.status(404).json({ error: 'no_such_message' });
  if (!readableChannel(req, existing.channel_id)) {
    return res.status(404).json({ error: 'no_such_channel' });
  }
  if (existing.user_id !== req.user.id) {
    return res.status(403).json({
      error: 'forbidden',
      message: 'You can only change your own messages.',
    });
  }

  const result = chat.setAttachment(id, {
    hash: req.body?.hash ?? null,
    name: req.body?.name ?? null,
  });
  if (!result.ok) {
    const messages = {
      no_such_upload: 'Upload the file first.',
      empty_message: 'Removing the file would leave nothing. Write something first.',
      no_such_message: 'No such message.',
    };
    return res.status(result.error === 'no_such_message' ? 404 : 400)
      .json({ error: result.error, message: messages[result.error] });
  }

  const message = oneForClient(result.message);
  toChannelReaders(existing.channel_id, { type: 'message:updated', message });
  return res.json({ message });
});

app.post('/api/messages/:id/delete', requireLogin, (req, res) => {
  const id = Number.parseInt(req.params.id, 10);

  // Checked BEFORE deleting, obviously -- the first draft of this removed the
  // row and then decided whether it was allowed to.
  const existing = chat.get(id);
  if (!existing) return res.status(404).json({ error: 'no_such_message' });

  const isOwn = existing.user_id === req.user.id;
  if (!isOwn && req.user.role !== 'owner' && req.user.role !== 'admin') {
    return res.status(403).json({ error: 'forbidden', message: 'Not your message.' });
  }

  const result = chat.remove(id);
  if (!result.ok) return res.status(404).json({ error: result.error });
  toChannelReaders(existing.channel_id, {
    type: 'message:deleted', id, channelId: existing.channel_id,
  });
  return res.json({ ok: true });
});

/**
 * React, or take it back. One route, because it is one gesture.
 *
 * No permission check beyond being signed in and able to read the channel:
 * a reaction is the cheapest thing anybody can say, and the only person it
 * can be removed by is the one who left it.
 */
app.post('/api/messages/:id/react', requireLogin, (req, res) => {
  const id = Number.parseInt(req.params.id, 10);
  const existing = chat.get(id);
  if (!existing) return res.status(404).json({ error: 'no_such_message' });
  if (!readableChannel(req, existing.channel_id)) {
    return res.status(404).json({ error: 'no_such_channel' });
  }

  const result = chat.react({
    messageId: id,
    userId: req.user.id,
    emoji: req.body?.emoji,
    on: req.body?.on !== false,
  });
  if (!result.ok) {
    return res.status(result.error === 'no_such_message' ? 404 : 400).json({
      error: result.error,
      message: result.error === 'bad_emoji'
        ? 'React with an emoji or a :name:, not a paragraph.'
        : 'No such message.',
    });
  }

  toChannelReaders(existing.channel_id, {
    type: 'message:reactions',
    id,
    channelId: existing.channel_id,
    reactions: result.reactions,
  });
  return res.json({ reactions: result.reactions });
});

app.get('/api/channels/:id/search', requireLogin, (req, res) => {
  const id = Number.parseInt(req.params.id, 10);
  const channel = readableChannel(req, id);
  if (!channel) return res.status(404).json({ error: 'no_such_channel' });

  const { mode, results } = chat.search(id, req.query.q);
  return res.json({ mode, results: forClient(results) });
});

/**
 * Uploads per person: a burst for somebody adding a set of emoji or a few
 * screenshots, then one every five seconds. Each can be 25 MB written to an
 * SD card, and the quota only stops the disk filling, not the churn.
 *
 * A piece of a big file spends its share of a token: its size over the
 * one-request cap, so a file in pieces costs about what the same bytes would
 * as whole uploads -- 20 x 25 MB at once, then 5 MB a second -- and a 2 GB
 * file is paced rather than turned away.
 */
const uploadThrottle = new Throttle({ burst: 20, perSecond: 0.2 });

/** Why an upload was refused, as an HTTP answer. */
const UPLOAD_REFUSALS = {
  empty_file: () => [400, 'That file is empty.'],
  type_not_allowed: () => [400, `Allowed types: ${allowedTypes().join(', ')}`],
  file_too_large: () => [413, `Files on this server are limited to ${formatBytes(chat.maxFileBytes)}.`],
  server_full: () => [507, 'The server has run out of space for uploads.'],
  disk_full: () => [507, "The server's disk is nearly full, so it is not taking files."],
  too_many_uploads: () => [429, 'Finish or cancel your other uploads first.'],
  no_such_upload: () => [404, 'That upload has expired. Start it again.'],
  bad_offset: () => [409, 'The upload lost its place.'],
  chunk_too_large: () => [413, 'That piece of the file is too big.'],
  incomplete: () => [409, 'The file has not all arrived yet.'],
  aborted: () => [400, 'The upload was interrupted.'],
};

function refuseUpload(res, result) {
  const [status, message] = (UPLOAD_REFUSALS[result.error] ?? (() => [400, 'Upload refused.']))();
  return res.status(status).json({
    error: result.error,
    message,
    ...(result.received !== undefined ? { received: result.received } : {}),
  });
}

function formatBytes(bytes) {
  if (bytes >= 1024 ** 3) return `${+(bytes / 1024 ** 3).toFixed(1)} GB`;
  return `${Math.round(bytes / 1024 ** 2)} MB`;
}

/** The throttle's answer as a 429, or null to go on. */
function uploadGate(req, res, cost = 1) {
  const gate = uploadThrottle.take(req.user.id, cost);
  if (gate.allowed) return null;
  if (!req.complete) res.set('Connection', 'close');
  res.set('Retry-After', String(gate.retryAfterSec));
  return res.status(429).json({
    error: 'slow_down',
    retryAfterSec: gate.retryAfterSec,
    message: 'Too many uploads at once. Wait a moment and try again.',
  });
}

/**
 * Upload a file: the raw bytes as the body, its type as Content-Type.
 *
 * Read as a stream into a file rather than through express.raw, which held
 * the whole body in memory and left hashing and writing it to a synchronous
 * call -- 25 MB of both with every other request waiting. See Chat.receive.
 * The global JSON parser is content-type gated, so these bodies pass through
 * it untouched.
 *
 * A type off the allowlist is refused unread as `empty_file`, which is what
 * express.raw's type filter used to make of it.
 */
app.post('/api/uploads', requireLogin, async (req, res) => {
  // A refusal before the body is all read hangs up after answering. Node
  // would otherwise read the rest to throw it away, and a body sent in
  // chunks has no length to stop at.
  const unread = () => { if (!req.complete) res.set('Connection', 'close'); };
  const refuse = (status, body) => {
    unread();
    return res.status(status).json(body);
  };
  const tooLarge = () => refuse(413, {
    error: 'file_too_large',
    message: chat.requestLimit() < chat.maxFileBytes
      ? `Files are limited to ${formatBytes(chat.requestLimit())} in one request. `
        + 'Newer versions of Ciranda send bigger ones in pieces.'
      : `Files are limited to ${formatBytes(chat.requestLimit())}.`,
  });

  if (uploadGate(req, res)) return undefined;

  const type = req.get('content-type')?.split(';')[0]?.trim().toLowerCase();
  if (!mediaTypeOf(type)) {
    return refuse(400, {
      error: 'empty_file',
      message: `Send the file as a raw body with one of: ${allowedTypes().join(', ')}`,
    });
  }
  // Stored as it arrives, so a compressed body would be stored compressed.
  // No client sends one.
  if ((req.get('content-encoding') ?? 'identity').toLowerCase() !== 'identity') {
    return refuse(415, { error: 'unsupported_encoding', message: 'Send the file uncompressed.' });
  }
  // Said up front by every client that knows the size, so it is refused
  // before a byte is read.
  if (Number(req.get('content-length')) > chat.requestLimit()) return tooLarge();

  let result;
  try {
    result = await chat.receive(req, type);
  } catch (err) {
    unread();
    throw err;
  }
  if (!result.ok) {
    if (result.error === 'aborted') return undefined;
    if (result.error === 'file_too_large') return tooLarge();
    unread();
    if (result.error === 'empty_file') {
      return res.status(400).json({
        error: result.error,
        message: `Send the file as a raw body with one of: ${allowedTypes().join(', ')}`,
      });
    }
    return refuseUpload(res, result);
  }

  return res.status(201).json(uploadReply(result));
});

/** What a stored upload is answered with, however it arrived. */
const uploadReply = (result) => ({
  hash: result.upload.hash,
  contentType: result.upload.content_type,
  bytes: result.upload.bytes,
  deduplicated: result.deduplicated,
  usedBytes: chat.usedBytes,
  quotaBytes: chat.quotaBytes,
});

/*
 * Upload sessions: a file of any size up to the owner's limit, in pieces,
 * straight to disk. See upload-sessions.js for the shape and the reasons.
 *
 * Opening one spends a whole token of the upload throttle, as a POST to
 * /api/uploads does; each piece then spends its share by size.
 */
app.post('/api/uploads/sessions', requireLogin, async (req, res) => {
  if (uploadGate(req, res)) return undefined;
  const result = await uploads.open({
    userId: req.user.id,
    size: Number(req.body?.size),
    contentType: req.body?.contentType,
  });
  if (!result.ok) return refuseUpload(res, result);
  return res.status(201).json({ id: result.id, chunkBytes: result.chunkBytes });
});

/*
 * One piece, at ?offset=. The raw body, unparsed: the JSON parser only reads
 * application/json, and a piece is sent as application/octet-stream. A
 * refusal before the piece is read hangs up after answering, as
 * /api/uploads does, rather than read 8 MB only to throw them away.
 */
app.post('/api/uploads/sessions/:id/chunk', requireLogin, async (req, res) => {
  const declared = Number(req.get('content-length'));
  const cost = (Number.isFinite(declared) && declared > 0 ? declared : CHUNK_BYTES) / MAX_UPLOAD_BYTES;
  if (uploadGate(req, res, Math.min(1, cost))) return undefined;
  if ((req.get('content-encoding') ?? 'identity').toLowerCase() !== 'identity') {
    res.set('Connection', 'close');
    return res.status(415).json({ error: 'unsupported_encoding', message: 'Send the file uncompressed.' });
  }
  const result = await uploads.append({
    id: String(req.params.id),
    userId: req.user.id,
    offset: Number(req.query.offset),
    body: req,
  });
  if (!result.ok) {
    if (result.error === 'aborted') return undefined;
    if (!req.complete) res.set('Connection', 'close');
    return refuseUpload(res, result);
  }
  return res.json({ received: result.received });
});

app.post('/api/uploads/sessions/:id/finish', requireLogin, async (req, res) => {
  const result = await uploads.finish({ id: String(req.params.id), userId: req.user.id });
  if (!result.ok) return refuseUpload(res, result);
  return res.status(201).json(uploadReply(result));
});

app.post('/api/uploads/sessions/:id/cancel', requireLogin, async (req, res) => {
  await uploads.cancel({ id: String(req.params.id), userId: req.user.id });
  return res.json({ ok: true });
});

/**
 * Serve a stored file.
 *
 * Always with the content type recorded at upload, never one the request
 * asks for, and always as an attachment-safe type: a file smuggled in as
 * image/png is served as image/png and cannot execute. X-Content-Type-Options
 * stops a browser sniffing its way to a different conclusion.
 */
app.get('/api/uploads/:hash', requireLogin, (req, res) => sendUpload(req, res));

/** A key for loading uploads without headers. See GET /api/media/:hash. */
app.get('/api/media-key', requireLogin, (req, res) => {
  res.json({ key: mintMediaKey(mediaKeySecret(req.user), req.user.id) });
});

/**
 * The body of GET /api/uploads/:hash, and of GET /api/media/:hash.
 *
 * `scope` is the Cache-Control audience. A media URL carries its key, and a
 * shared cache told to ignore query strings would hand the file to anybody
 * with the bare path -- so that one is `private`: the browser still keeps it.
 */
function sendUpload(req, res, { scope = 'public' } = {}) {
  const hash = String(req.params.hash);
  if (!/^[0-9a-f]{64}$/.test(hash)) return res.status(400).json({ error: 'bad_hash' });

  // A private conversation's file is served by that conversation's own route,
  // to its two people; here it does not exist, whoever asks and whatever they know.
  const info = chat.isSealed(hash) ? null : chat.fileInfo(hash);
  if (!info) return res.status(404).json({ error: 'no_such_file' });

  res.set('Content-Type', info.content_type);
  res.set('X-Content-Type-Options', 'nosniff');
  res.set('Content-Security-Policy', "default-src 'none'; sandbox");
  // Content-addressed, so it can never change: cache it forever.
  res.set('Cache-Control', `${scope}, max-age=31536000, immutable`);
  return res.sendFile(info.path, STORED_FILE);
}

// ---------------------------------------------------------------------------
// Private conversations: see dm-routes.js
// ---------------------------------------------------------------------------

mountDirectMessages(app, {
  requireLogin,
  dms,
  chat,
  accounts,
  push: (userIds, payload) => { for (const id of userIds) realtime?.toUser(id, payload); },
  broadcast: (payload) => realtime?.broadcast(payload),
});

/**
 * Whether anything arrived, for a client that keeps this server in the
 * background with no socket open: the newest private message and whether a
 * call is ringing for you. Cheap on purpose, since it is asked every 15
 * seconds; the client opens a socket only once the answer changes. Asking
 * also keeps you reachable, so a call to you rings instead of being missed.
 */
app.get('/api/inbox', requireLogin, (req, res) => {
  realtime?.notePoll(req.user.id);
  const mine = calls.forUser(req.user.id);
  res.json({
    latest: dms.latest(req.user.id),
    ringing: mine.some((c) => c.state === 'ringing' && c.calleeId === req.user.id),
    inCall: mine.some((c) => c.state === 'active'),
  });
});

// ---------------------------------------------------------------------------
// Custom emoji
//
// Any member may add one; the uploader or an admin may remove it. See the
// Emojis class for why this is looser than the soundpad.
// ---------------------------------------------------------------------------

app.get('/api/emojis', requireLogin, (_req, res) => {
  res.json({ emojis: emojis.list().map(publicEmoji) });
});

app.post('/api/emojis', requireLogin, (req, res) => {
  const result = emojis.add({
    name: req.body?.name,
    fileHash: req.body?.hash,
    userId: req.user.id,
  });
  if (!result.ok) {
    const messages = {
      invalid_name: 'Names are 2-32 characters of a-z, 0-9 and _ -- it becomes the :trigger:.',
      name_taken: 'Something else is already called that.',
      too_many_emojis: `This server is at its limit of ${MAX_EMOJIS} custom emoji.`,
      no_such_upload: 'Upload the picture first.',
      not_an_image: 'A custom emoji has to be an image.',
      emoji_too_large:
        `Custom emoji are limited to ${Math.round(MAX_EMOJI_BYTES / 1024)} KB -- `
        + 'every client downloads every one of them.',
    };
    return res.status(400).json({ error: result.error, message: messages[result.error] });
  }
  const list = emojis.list().map(publicEmoji);
  realtime?.broadcast({ type: 'emojis', emojis: list });
  return res.status(201).json({ emoji: publicEmoji(result.emoji) });
});

app.post('/api/emojis/:id/delete', requireLogin, (req, res) => {
  const id = Number.parseInt(req.params.id, 10);

  // Checked before removing, for the same reason deleting a message is.
  const existing = emojis.get(id);
  if (!existing) return res.status(404).json({ error: 'no_such_emoji' });

  const isOwn = existing.uploaded_by === req.user.id;
  if (!isOwn && req.user.role !== 'owner' && req.user.role !== 'admin') {
    return res.status(403).json({ error: 'forbidden', message: 'Not yours to remove.' });
  }

  const result = emojis.remove(id);
  if (!result.ok) return res.status(404).json({ error: result.error });
  realtime?.broadcast({ type: 'emojis', emojis: emojis.list().map(publicEmoji) });
  return res.json({ ok: true });
});

// ---------------------------------------------------------------------------
// Soundpad
// ---------------------------------------------------------------------------

app.get('/api/soundpad', requireLogin, (_req, res) => {
  res.json({ clips: soundpad.list().map(publicClip) });
});

app.post('/api/soundpad', requireAdmin, (req, res) => {
  const result = soundpad.add({
    name: req.body?.name,
    emoji: req.body?.emoji,
    fileHash: req.body?.hash,
    userId: req.user.id,
  });
  if (!result.ok) {
    const messages = {
      invalid_name: 'Give the clip a name.',
      no_such_upload: 'Upload the audio first.',
      not_audio: 'Soundpad clips have to be audio.',
      clip_too_large:
        `Soundpad clips are limited to ${Math.round(MAX_CLIP_BYTES / 1024 / 1024)} MB -- `
        + 'every client downloads every clip.',
    };
    return res.status(400).json({ error: result.error, message: messages[result.error] });
  }
  realtime?.broadcast({ type: 'soundpad', clips: soundpad.list().map(publicClip) });
  return res.status(201).json({ clip: publicClip(result.clip) });
});

// Before /api/soundpad/:id/delete only by habit -- they cannot collide, since
// that one has a second path segment. The ordering rule still applies to the
// next person who adds /api/soundpad/:id, so it stays up here.
app.post('/api/soundpad/reorder', requireAdmin, (req, res) => {
  const result = soundpad.reorder(Array.isArray(req.body?.ids) ? req.body.ids : []);
  if (!result.ok) {
    return res.status(400).json({
      error: result.error,
      message: 'Send every clip id exactly once, in the order you want.',
    });
  }
  const clips = result.clips.map(publicClip);
  realtime?.broadcast({ type: 'soundpad', clips });
  return res.json({ clips });
});

app.post('/api/soundpad/:id/rename', requireAdmin, (req, res) => {
  const result = soundpad.rename(
    Number.parseInt(req.params.id, 10), req.body?.name, req.body?.emoji,
  );
  if (!result.ok) {
    const messages = {
      no_such_clip: 'No such clip.',
      invalid_name: 'Give the clip a name.',
    };
    return res.status(400).json({ error: result.error, message: messages[result.error] });
  }
  realtime?.broadcast({ type: 'soundpad', clips: soundpad.list().map(publicClip) });
  return res.json({ clip: publicClip(result.clip) });
});

app.post('/api/soundpad/:id/delete', requireAdmin, (req, res) => {
  const result = soundpad.remove(Number.parseInt(req.params.id, 10));
  if (!result.ok) return res.status(404).json({ error: result.error });
  realtime?.broadcast({ type: 'soundpad', clips: soundpad.list().map(publicClip) });
  return res.json({ ok: true });
});

// ---------------------------------------------------------------------------
// MediaMTX auth hook
//
// MediaMTX POSTs { user, password, token, ip, action, path, protocol, id,
// query, userAgent } and reads only the status code: 2xx allows, anything else
// denies.
// ---------------------------------------------------------------------------

app.post('/mediamtx/auth', (req, res) => {
  const { action, path, query } = req.body ?? {};

  // api / metrics / pprof are already excluded in mediamtx.yml, but MediaMTX
  // will ask if that config is ever edited. Loopback only.
  if (action === 'api' || action === 'metrics' || action === 'pprof') {
    return res.sendStatus(204);
  }

  /**
   * CHANNEL PATHS ARE HANDLED FIRST, AND THAT ORDERING IS SECURITY-CRITICAL.
   *
   * Two things below this point would otherwise defeat channel passwords
   * entirely:
   *
   *   1. the legacy read branch accepts the single process-wide `mediaToken`,
   *      which every client gets just for knowing the server password -- so a
   *      legacy client could listen to a password-protected voice channel;
   *   2. the open-server shortcut (`if (!config.mediaToken) return 204`) lets
   *      ANY read through on a server with no HARMONY_PASSWORD -- so a channel
   *      password would mean nothing at all there.
   *
   * Neither is a bug in those branches; they are correct for the flat username
   * namespace. They are simply the wrong answer for `vc-...`, and the only
   * thing keeping them from being asked is this early return. Do not "simplify"
   * it downwards.
   */
  const channelTarget = parseChannelPath(path);
  if (channelTarget) {
    const token = new URLSearchParams(query ?? '').get('token');
    const claim = readChannelToken(mediaSecret, token);

    if (!claim || claim.cid !== channelTarget.cid || claim.flags !== TOKEN_FLAGS.channel) {
      console.warn(`[auth] channel ${action} REJECTED for "${path}" (bad or expired token)`);
      return res.sendStatus(401);
    }

    if (action === 'read' || action === 'playback') {
      // Membership of the channel is the whole check: any member may watch any
      // other member. The hook never consults live presence, which keeps it
      // stateless, restart-proof, and off any lock during a join storm.
      return res.sendStatus(204);
    }

    if (action === 'publish') {
      // Publishing additionally requires the SLOT to match, or one member
      // could publish into another member's path.
      if (claim.mid !== channelTarget.mid) {
        console.warn(`[auth] channel publish REJECTED for "${path}" (slot mismatch)`);
        return res.sendStatus(401);
      }
      // The enforcing half of force-mute. The Phase 0 spike showed that
      // kicking the live session alone does nothing lasting -- the path is
      // immediately re-publishable -- so this refusal is what makes a mute
      // stick across the reconnect that follows.
      if (channelTarget.kind === MEDIA_KINDS.voice
          && voice.isForceMuted(channelTarget.cid, channelTarget.mid)) {
        console.warn(`[auth] channel publish REFUSED for "${path}" (force-muted)`);
        return res.sendStatus(401);
      }
      return res.sendStatus(204);
    }

    return res.sendStatus(401);
  }

  /*
   * A private call's paths. Same token format and secret as a channel's, told
   * apart by the flag: a token minted for channel 5 must not open call 5.
   * Only the call's two people are ever issued one, and the frames inside are
   * sealed end to end anyway -- this keeps strangers off the relay, it is not
   * what keeps the call private.
   */
  const callTarget = parseCallPath(path);
  if (callTarget) {
    const token = new URLSearchParams(query ?? '').get('token');
    const claim = readChannelToken(mediaSecret, token);
    if (!claim || claim.cid !== callTarget.conversationId || claim.flags !== TOKEN_FLAGS.call) {
      console.warn(`[auth] call ${action} REJECTED for "${path}" (bad or expired token)`);
      return res.sendStatus(401);
    }
    if (action === 'read' || action === 'playback') return res.sendStatus(204);
    if (action === 'publish' && claim.mid === callTarget.mid) return res.sendStatus(204);
    console.warn(`[auth] call ${action} REJECTED for "${path}" (slot mismatch)`);
    return res.sendStatus(401);
  }

  // normalizePath, NOT normalizeUsername: `bob-cam` is a system path that is
  // refused as user input but is legitimate here. Using the user-input
  // validator in the auth hook is what would make the two namespaces collide.
  const username = normalizePath(path);
  if (!username) return res.sendStatus(401);

  // On an open server, watching is open to anyone who knows the name -- that is
  // the design. With a password configured it must not be: MediaMTX listens on
  // its own port, so a reader who never touched the control server would other-
  // wise walk straight past the password. The watch URLs handed out by
  // /api/streams carry the token that proves the holder got them from us.
  if (action === 'read' || action === 'playback') {
    if (!config.mediaToken) return res.sendStatus(204);
    const token = new URLSearchParams(query ?? '').get('token');
    if (secretsMatch(token, config.mediaToken)) return res.sendStatus(204);
    console.warn(`[auth] read REJECTED for "${username}" (missing or stale watch token)`);
    return res.sendStatus(401);
  }

  if (action === 'publish') {
    // MediaMTX forwards the raw query string from the WHIP URL.
    const token = new URLSearchParams(query ?? '').get('token');
    if (rooms.mayPublish(username, token)) {
      console.log(`[auth] publish accepted for "${username}"`);
      return res.sendStatus(204);
    }
    console.warn(`[auth] publish REJECTED for "${username}" (bad or expired token)`);
    return res.sendStatus(401);
  }

  return res.sendStatus(401);
});

// ---------------------------------------------------------------------------

/**
 * The media URLs and tokens a member needs for one channel.
 *
 * One token covers reading and publishing; see channels.js for why that is
 * safe. The client builds every peer's WHEP URL from `readToken` and its own
 * WHIP URLs from the paths here. `host` is the Host header the member's
 * socket came in with, for HARMONY_SIGNALING_URL=auto.
 */
function issueTokens(channelId, userId, mid, host) {
  return mediaTokens({ id: channelId, mid, host, flags: TOKEN_FLAGS.channel, pathFor: channelPath });
}

/** The same, for one side of a private call: its own namespace and its own flag. */
function issueCallTokens(conversationId, userId, mid, host) {
  return mediaTokens({ id: conversationId, mid, host, flags: TOKEN_FLAGS.call, pathFor: callPath });
}

function mediaTokens({ id, mid, host, flags, pathFor }) {
  const token = mintChannelToken(mediaSecret, {
    cid: id, mid, flags, ttlMs: config.channelTokenTtlMs,
  });
  const base = signalingBase(host);
  const url = (kind) => `${base}/${pathFor(id, mid, kind)}`;
  return {
    token,
    publish: {
      voice: `${url('voice')}/whip?token=${encodeURIComponent(token)}`,
      cam: `${url('cam')}/whip?token=${encodeURIComponent(token)}`,
      screen: `${url('screen')}/whip?token=${encodeURIComponent(token)}`,
    },
    // A template rather than a list: the roster changes constantly and the
    // client already knows every member's slot from it.
    whepBase: base,
    /*
     * How long this token lasts, so the client can come back before it does
     * not. It is a HINT and not a contract -- the client halves it and the
     * server re-issues on request, so neither side has to agree on a clock.
     *
     * Sending this and never acting on it is precisely the bug that shipped:
     * a channel's tokens were minted once at join and never renewed, so
     * after ten minutes no new subscription, camera or screen share in that
     * channel could be authorised. Nothing in flight broke, which is what
     * made it look like "sometimes I cannot hear someone".
     */
    expiresInMs: config.channelTokenTtlMs,
  };
}

monitor.start();

// Sweep idle sessions hourly. Cheap, and it keeps a long-lived server from
// accumulating a row per login forever.
const sessionSweeper = setInterval(
  () => accounts.expireSessions(config.sessionIdleMs),
  60 * 60 * 1000,
);
sessionSweeper.unref();

// What a handler threw or rejected with: logged, and a plain 500 rather than
// Express's HTML page with a stack trace in it. Express 5 brings an async
// handler's rejection here too; on 4 it was an unhandled rejection, and Node
// ends the process on one.
app.use((err, req, res, next) => {
  if (res.headersSent) return next(err);
  // The body parser's own refusals (too large, not JSON) keep their status.
  if (err?.status >= 400 && err.status < 500) return res.status(err.status).json({ error: err.type ?? 'bad_request' });
  console.warn(`[http] ${req.method} ${req.path} failed: ${err?.stack ?? err}`);
  return res.status(500).json({ error: 'server_error' });
});

let realtime = null;

const server = app.listen(config.port, config.host, (err) => {
  // Express 5 hands a failure to listen (the port taken, say) to this
  // callback instead of throwing it. Carrying on would leave a process that
  // looks up and answers nothing.
  if (err) {
    console.error(`[harmony] cannot listen on ${config.host}:${config.port}: ${err.message}`);
    process.exit(1);
  }
  console.log(`[harmony] control server on http://${config.host}:${config.port}`);
  console.log(config.signaling.auto
    ? `[harmony] clients will be sent to the host they connect to, as ${config.signalingBase.replace('localhost', '<host>')}`
    : `[harmony] clients will be sent to ${config.signalingBase}`);
  // serverSettings, not config: the owner may have changed the door from
  // the client, and a banner that reports the environment would be lying
  // about the server it just started.
  console.log(
    serverSettings.passwordRequired
      ? `[harmony] password required — ${config.maxLoginAttempts} tries, then ${config.lockoutMinutes.join('/')} minute lockouts`
      : '[harmony] NO PASSWORD SET — anyone who can reach this server can use it',
  );
  console.log(`[harmony] this server is called ${serverSettings.name}`);
});

realtime = new Realtime({
  server,
  accounts,
  channels,
  voice,
  soundpad,
  dms,
  calls,
  issueTokens,
  issueCallTokens,
  kickMember: (channelId, mid) => monitor.kickPath(channelPath(channelId, mid, 'voice')),
});

/**
 * Push the live stream list instead of having every client poll for it.
 *
 * Only on CHANGE: the monitor ticks once a second and the list is usually
 * identical, so broadcasting every tick would be the same poll with extra
 * steps. The client keeps a slow reconciliation poll as a safety net.
 */
let lastStreamsJson = '';
monitor.on('paths', () => {
  const streams = rooms.listLive();
  const json = JSON.stringify(streams);
  if (json === lastStreamsJson) return;
  lastStreamsJson = json;
  realtime.broadcastStreams((host) => {
    const base = signalingBase(host);
    return streams.map((s) => ({ ...s, whepUrl: whepUrl(s.username, base) }));
  });
});

for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => {
    console.log(`\n[harmony] ${signal} -- shutting down`);
    monitor.stop();
    calls.close();
    realtime?.close();
    clearInterval(sessionSweeper);
    server.close(() => {
      // Closing checkpoints the WAL, so the next start does not have to replay
      // it. Skipping this is survivable but leaves -wal/-shm files behind.
      db.close();
      process.exit(0);
    });
  });
}
