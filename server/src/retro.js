// The year in review: what a server's people did together in a year, worked out from what is
// already kept (messages, reactions) and from the journal (journal.js: hours in voice, games,
// songs), for whoever asks -- the server's numbers and their own.
//
// Everything is counted in the asker's local time (`tzMinutes`, what their clock is ahead of UTC),
// so "the night owl" is whoever writes between midnight and five where the asker lives. On a
// friends' server that is everybody's clock, near enough.
//
// The journal began on the day this shipped, so the hours are "since" its first row; the reply
// says when that was, and the client says so rather than pretending to a whole year.

const HOUR = 3_600_000;
const DAY = 24 * HOUR;
/** How many of each list are sent. */
const TOP = 5;
/** Midnight to five in the morning: the night owl's hours. */
const NIGHT = [0, 5];

/**
 * @param {import('node:sqlite').DatabaseSync} db
 * @param {{year: number, tzMinutes: number, userId: number, now?: number}} opts
 */
export function yearInReview(db, { year, tzMinutes, userId, now = Date.now() }) {
  const off = tzMinutes * 60_000;
  // The year's bounds in the asker's time, as UTC milliseconds.
  const from = Date.UTC(year, 0, 1) - off;
  const to = Math.min(Date.UTC(year + 1, 0, 1) - off, now);
  const all = (sql, ...args) => db.prepare(sql).all(...args);
  const one = (sql, ...args) => db.prepare(sql).get(...args);

  // Only people still here, and not banned: a removed account's id may belong to somebody else.
  const people = new Set(all('SELECT id FROM users WHERE banned_at IS NULL').map((r) => r.id));
  const known = (id) => people.has(id);

  // --------------------------------------------------------------- messages
  const messages = all(
    'SELECT m.id, m.user_id AS userId, m.channel_id AS channelId, m.created_at AS at, m.body '
    + 'FROM messages m WHERE m.created_at >= ? AND m.created_at < ?',
    from, to,
  ).filter((m) => known(m.userId));

  const byUser = count(messages, (m) => m.userId);
  const byChannel = count(messages, (m) => m.channelId);
  const byDay = count(messages, (m) => dayOf(m.at, off));
  const night = messages.filter((m) => {
    const h = hourOf(m.at, off);
    return h >= NIGHT[0] && h < NIGHT[1];
  });
  const nightByUser = count(night, (m) => m.userId);
  const byHour = new Array(24).fill(0);
  for (const m of messages) byHour[hourOf(m.at, off)] += 1;

  const channelNames = new Map(all('SELECT id, name FROM channels').map((c) => [c.id, c.name]));
  const chatters = ranked(byUser);
  const busiestChannel = ranked(byChannel).find(([id]) => channelNames.has(id));
  const busiestDay = ranked(byDay)[0];

  // --------------------------------------------------------------- reactions
  const reactions = all(
    'SELECT r.user_id AS userId, r.emoji FROM message_reactions r WHERE r.created_at >= ? AND r.created_at < ?',
    from, to,
  ).filter((r) => known(r.userId));
  const topReactions = ranked(count(reactions, (r) => r.emoji)).slice(0, TOP).map(([emoji, n]) => ({ emoji, n }));

  // --------------------------------------------------------------- voice
  const sessions = all(
    'SELECT user_id AS userId, channel_id AS channelId, started_at AS a, ended_at AS b FROM voice_sessions '
    + 'WHERE ended_at > ? AND started_at < ?',
    from, to,
  )
    .filter((s) => known(s.userId))
    .map((s) => ({ ...s, a: Math.max(s.a, from), b: Math.min(s.b, to) }))
    .filter((s) => s.b > s.a);
  const voiceByUser = sum(sessions, (s) => s.userId, (s) => s.b - s.a);
  const voiceMs = [...voiceByUser.values()].reduce((x, y) => x + y, 0);
  const pairs = overlaps(sessions);
  const hangout = longestHangout(sessions);

  // --------------------------------------------------------------- games
  const plays = all(
    'SELECT user_id AS userId, name, started_at AS a, ended_at AS b FROM activity_sessions WHERE ended_at > ? AND started_at < ?',
    from, to,
  )
    .filter((s) => known(s.userId))
    .map((s) => ({ ...s, a: Math.max(s.a, from), b: Math.min(s.b, to) }))
    .filter((s) => s.b > s.a);
  const gameMs = sum(plays, (s) => s.name, (s) => s.b - s.a);
  const gamePlayers = new Map();
  for (const s of plays) {
    if (!gamePlayers.has(s.name)) gamePlayers.set(s.name, new Set());
    gamePlayers.get(s.name).add(s.userId);
  }
  const gamerMs = sum(plays, (s) => s.userId, (s) => s.b - s.a);

  // --------------------------------------------------------------- Cirandinha
  const songs = all(
    'SELECT title, artist, added_by AS addedBy FROM music_plays WHERE played_at >= ? AND played_at < ?',
    from, to,
  );
  const songCounts = new Map();
  for (const s of songs) {
    const key = `${s.title}\u0000${s.artist}`;
    const was = songCounts.get(key) ?? { title: s.title, artist: s.artist, n: 0 };
    was.n += 1;
    songCounts.set(key, was);
  }
  const djs = ranked(count(songs.filter((s) => s.addedBy), (s) => s.addedBy));

  // --------------------------------------------------------------- since when the journal knows
  const firstOf = (table, column) => one(`SELECT MIN(${column}) AS at FROM ${table}`)?.at ?? null;
  const since = {
    voice: firstOf('voice_sessions', 'started_at'),
    games: firstOf('activity_sessions', 'started_at'),
    music: firstOf('music_plays', 'played_at'),
  };

  // --------------------------------------------------------------- yours
  const mine = messages.filter((m) => m.userId === userId);
  const myRank = chatters.findIndex(([id]) => id === userId);
  const myChannel = ranked(count(mine, (m) => m.channelId)).find(([id]) => channelNames.has(id));
  const first = mine.reduce((a, m) => (!a || m.at < a.at ? m : a), null);
  const myReaction = ranked(count(reactions.filter((r) => r.userId === userId), (r) => r.emoji))[0];
  const myGames = ranked(sum(plays.filter((s) => s.userId === userId), (s) => s.name, (s) => s.b - s.a));
  const myPartner = pairs
    .filter((p) => p.a === userId || p.b === userId)
    .map((p) => ({ userId: p.a === userId ? p.b : p.a, ms: p.ms }))[0] ?? null;

  return {
    year,
    since,
    server: {
      messages: messages.length,
      writers: byUser.size,
      topChatters: chatters.slice(0, TOP).map(([id, n]) => ({ userId: id, n })),
      busiestChannel: busiestChannel ? { channelId: busiestChannel[0], name: channelNames.get(busiestChannel[0]), n: busiestChannel[1] } : null,
      busiestDay: busiestDay ? { date: busiestDay[0], n: busiestDay[1] } : null,
      byHour,
      nightOwl: ranked(nightByUser)[0] ? { userId: ranked(nightByUser)[0][0], n: ranked(nightByUser)[0][1] } : null,
      topReactions,
      reactions: reactions.length,
      voiceMs,
      topTalkers: ranked(voiceByUser).slice(0, TOP).map(([id, ms]) => ({ userId: id, ms })),
      topPair: pairs[0] ?? null,
      longestHangout: hangout ? { ...hangout, name: channelNames.get(hangout.channelId) ?? null } : null,
      games: ranked(gameMs).slice(0, TOP).map(([name, ms]) => ({ name, ms, players: gamePlayers.get(name)?.size ?? 0 })),
      topGamer: ranked(gamerMs)[0] ? { userId: ranked(gamerMs)[0][0], ms: ranked(gamerMs)[0][1] } : null,
      songsPlayed: songs.length,
      topSongs: [...songCounts.values()].sort((x, y) => y.n - x.n).slice(0, TOP),
      topDj: djs[0] ? { nickname: djs[0][0], n: djs[0][1] } : null,
    },
    me: {
      messages: mine.length,
      rank: myRank >= 0 ? myRank + 1 : null,
      topChannel: myChannel ? { channelId: myChannel[0], name: channelNames.get(myChannel[0]), n: myChannel[1] } : null,
      firstMessage: first ? { at: first.at, channelId: first.channelId, body: excerpt(first.body) } : null,
      nightMessages: nightByUser.get(userId) ?? 0,
      topReaction: myReaction ? { emoji: myReaction[0], n: myReaction[1] } : null,
      voiceMs: voiceByUser.get(userId) ?? 0,
      topGame: myGames[0] ? { name: myGames[0][0], ms: myGames[0][1] } : null,
      gameMs: myGames.reduce((x, [, ms]) => x + ms, 0),
      partner: myPartner,
    },
  };
}

// ------------------------------------------------------------------ helpers

function count(rows, key) {
  const out = new Map();
  for (const r of rows) out.set(key(r), (out.get(key(r)) ?? 0) + 1);
  return out;
}

function sum(rows, key, value) {
  const out = new Map();
  for (const r of rows) out.set(key(r), (out.get(key(r)) ?? 0) + value(r));
  return out;
}

/** A map's entries, biggest first; ties keep their first-seen order. */
function ranked(map) {
  return [...map].sort((x, y) => y[1] - x[1]);
}

function hourOf(at, off) {
  return Math.floor((((at + off) % DAY) + DAY) % DAY / HOUR);
}

function dayOf(at, off) {
  return new Date(at + off).toISOString().slice(0, 10);
}

function excerpt(body) {
  const text = String(body ?? '').replace(/\s+/g, ' ').trim();
  return [...text].length > 140 ? `${[...text].slice(0, 139).join('')}…` : text;
}

/**
 * Time each two people spent in the same voice channel at once, most first.
 * @returns {{a: number, b: number, ms: number}[]}
 */
export function overlaps(sessions) {
  const byChannel = new Map();
  for (const s of sessions) {
    if (!byChannel.has(s.channelId)) byChannel.set(s.channelId, []);
    byChannel.get(s.channelId).push(s);
  }
  const total = new Map();
  for (const list of byChannel.values()) {
    for (let i = 0; i < list.length; i += 1) {
      for (let j = i + 1; j < list.length; j += 1) {
        const [x, y] = [list[i], list[j]];
        if (x.userId === y.userId) continue;
        const ms = Math.min(x.b, y.b) - Math.max(x.a, y.a);
        if (ms <= 0) continue;
        const [a, b] = x.userId < y.userId ? [x.userId, y.userId] : [y.userId, x.userId];
        const key = `${a}:${b}`;
        total.set(key, { a, b, ms: (total.get(key)?.ms ?? 0) + ms });
      }
    }
  }
  return [...total.values()].sort((x, y) => y.ms - x.ms);
}

/**
 * The longest unbroken stretch with two people or more in one voice channel: when it began, how
 * long it lasted and everybody who passed through it.
 */
export function longestHangout(sessions) {
  const byChannel = new Map();
  for (const s of sessions) {
    if (!byChannel.has(s.channelId)) byChannel.set(s.channelId, []);
    byChannel.get(s.channelId).push(s);
  }
  let best = null;
  for (const [channelId, list] of byChannel) {
    // Joins before leaves at the same instant, so a hand-over keeps the stretch going.
    const edges = list.flatMap((s) => [{ at: s.a, d: 1, s }, { at: s.b, d: -1, s }])
      .sort((x, y) => x.at - y.at || y.d - x.d);
    let here = 0;
    let start = null;
    let who = new Set();
    const live = new Set();
    for (const e of edges) {
      if (e.d > 0) live.add(e.s);
      else live.delete(e.s);
      here += e.d;
      if (here >= 2 && start === null) {
        start = e.at;
        who = new Set([...live].map((s) => s.userId));
      } else if (here >= 2) {
        for (const s of live) who.add(s.userId);
      } else if (here < 2 && start !== null) {
        const ms = e.at - start;
        if (!best || ms > best.ms) best = { channelId, startedAt: start, ms, people: [...who] };
        start = null;
      }
    }
  }
  return best;
}
