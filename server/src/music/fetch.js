// The two things the music reads from the web itself, and nothing else: a video's picture from YouTube's image host,
// and a Spotify embed page for what it lists.
//
// Neither address comes from anybody's hands. Each is built here from an id that has been checked (classify.js) and a
// host that is fixed in the code (or, for the tests, by the operator's environment), so this is not a fetcher of
// arbitrary links and needs none of a link previewer's care about addresses inside a network. What it does guard
// against is the far end: an answer that never ends (a byte cap, counted after unpacking), one that never comes (a
// deadline), and a redirect somewhere else (refused: a redirect is followed only to the same host, and never from
// https down to http).

import http from 'node:http';
import https from 'node:https';
import zlib from 'node:zlib';

const TIMEOUT_MS = 8000;
const MAX_REDIRECTS = 3;
const USER_AGENT = 'Mozilla/5.0 (compatible; HarmonyBot/1.0; music)';

/** What one request answered: a body, or where it says to go instead. */
function once(target, { accept, maxBytes }) {
  return new Promise((resolve, reject) => {
    const client = target.protocol === 'https:' ? https : http;
    const request = client.request(target, {
      method: 'GET',
      headers: {
        'User-Agent': USER_AGENT,
        Accept: accept,
        'Accept-Language': 'en-US,en;q=0.8,pt-BR;q=0.6',
        'Accept-Encoding': 'gzip, deflate, br',
      },
    }, (res) => {
      const status = res.statusCode ?? 0;
      if (status >= 300 && status < 400 && res.headers.location) {
        res.resume();
        let next;
        try {
          next = new URL(res.headers.location, target);
        } catch {
          reject(new Error('a redirect to nowhere'));
          return;
        }
        resolve({ status, redirect: next });
        return;
      }

      const encoding = String(res.headers['content-encoding'] ?? '').toLowerCase();
      let stream = res;
      if (encoding === 'gzip' || encoding === 'x-gzip') stream = res.pipe(zlib.createGunzip());
      else if (encoding === 'deflate') stream = res.pipe(zlib.createInflate());
      else if (encoding === 'br') stream = res.pipe(zlib.createBrotliDecompress());

      const chunks = [];
      let have = 0;
      let done = false;
      const finish = (truncated) => {
        if (done) return;
        done = true;
        request.destroy();
        resolve({ status, body: Buffer.concat(chunks), truncated });
      };
      // The size is what is read AFTER it is unpacked: a few kilobytes can be a few gigabytes.
      stream.on('data', (chunk) => {
        if (done) return;
        const room = maxBytes - have;
        chunks.push(room < chunk.length ? chunk.subarray(0, room) : chunk);
        have += Math.min(chunk.length, room);
        if (have >= maxBytes) finish(true);
      });
      stream.on('end', () => finish(false));
      stream.on('error', (err) => {
        if (done) return;
        done = true;
        reject(err);
      });
    });
    request.setTimeout(TIMEOUT_MS, () => request.destroy(new Error('timed out')));
    request.on('error', reject);
    request.end();
  });
}

/**
 * Read one of the music's fixed addresses, up to a number of bytes.
 *
 * @param {string} url  built by the caller from a fixed host and a checked id
 * @returns {Promise<{status: number, body: Buffer, truncated: boolean}>}
 * @throws {Error} unreachable, too slow, or sent somewhere else
 */
export async function fetchFixed(url, { accept = '*/*', maxBytes = 256 * 1024 } = {}) {
  const first = new URL(url);
  if (first.protocol !== 'https:' && first.protocol !== 'http:') throw new Error('not a web address');
  let target = first;
  const deadline = Date.now() + TIMEOUT_MS * 2;
  for (let hops = 0; hops <= MAX_REDIRECTS; hops += 1) {
    if (Date.now() > deadline) throw new Error('timed out');
    // eslint-disable-next-line no-await-in-loop
    const result = await once(target, { accept, maxBytes });
    if (!result.redirect) return result;
    const next = result.redirect;
    if (next.hostname !== first.hostname || next.port !== first.port) throw new Error('redirected to another host');
    if (first.protocol === 'https:' && next.protocol !== 'https:') throw new Error('redirected off https');
    if (next.username || next.password) throw new Error('an address with a password');
    target = next;
  }
  throw new Error('too many redirects');
}

/** What kind of picture some bytes are, from their first bytes -- a server's say-so is not enough. */
export function sniffImage(bytes) {
  if (bytes.length > 12 && bytes[0] === 0x89 && bytes.toString('latin1', 1, 4) === 'PNG') return 'image/png';
  if (bytes.length > 3 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) return 'image/jpeg';
  if (bytes.length > 12 && bytes.toString('latin1', 0, 4) === 'RIFF' && bytes.toString('latin1', 8, 12) === 'WEBP') return 'image/webp';
  return null;
}
