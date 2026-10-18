// What the server is called, its picture, and the key to its front door.
//
// They live in server_meta rather than only in the environment, so the owner
// can change them from the client instead of editing a file on the host and
// restarting. The environment is still the default: a fresh server behaves
// exactly as it always did, and an override only exists once somebody sets
// one.

import { createHash, randomBytes, timingSafeEqual } from 'node:crypto';

import { secretsMatch } from './auth.js';

const NAME_KEY = 'server_name';
const DOOR_KEY = 'door_password';
const ICON_KEY = 'server_icon';
const LEGACY_LOGO_KEY = 'server_logo';
// The Electron line's key for the same setting, so a database that moves
// between the two lines keeps it.
const MAX_FILE_KEY = 'max_file_bytes';

/** What a server with no name of its own is called. */
export const DEFAULT_NAME = 'Harmony';
const MAX_NAME = 32;

/**
 * The stored form of a door password.
 *
 * SHA-256 with a random salt, NOT scrypt -- and that is the opposite of the
 * rule for account passwords, for a reason that is worth writing down. This
 * comparison happens on EVERY request, including the MediaMTX auth hook,
 * which runs on every publish and every read in the system. scrypt is 20 ms
 * here and 150 ms on a Pi; an event-loop stall of that size in the auth hook
 * is a visible stream failure, not a slow login.
 *
 * So this is a trade made with open eyes: the shared door key is protected
 * against a casual read of the database file, not against an offline attack
 * on a weak password. It is the same key that sits in plain text in
 * /etc/harmony/harmony.env today, and a salted digest is strictly better
 * than that -- but it is not an account password and must never be reused
 * as one.
 */
function hashDoor(password, salt) {
  return createHash('sha256').update(`${salt}:${password}`).digest('hex');
}

export class ServerSettings {
  #meta;
  #envPassword;
  #relayGuarded;

  /**
   * `relayGuarded()` says whether the legacy media branch is asking for its
   * watch token right now (see restartRequired). Left out, it is what it was
   * at boot: whether the environment set a password.
   *
   * @param {{get: (k: string) => string|null, set: (k: string, v: string) => void,
   *          delete: (k: string) => void}} meta
   * @param {{envPassword: string, relayGuarded?: () => boolean}} opts
   */
  constructor(meta, { envPassword = '', relayGuarded = null } = {}) {
    this.#meta = meta;
    this.#envPassword = envPassword;
    this.#relayGuarded = relayGuarded ?? (() => Boolean(this.#envPassword));

    // A server that ran the other line kept its picture as `server_logo`.
    // Same thing, an upload hash holding one reference, so it moves across
    // as it is -- reference included -- rather than living under two names.
    // Should both be set, the icon wins and the logo's file keeps a stray
    // reference: one small picture that is never evicted, not worth an
    // uploads module in here to release it.
    const logo = meta.get(LEGACY_LOGO_KEY);
    if (logo) {
      if (!this.iconHash) meta.set(ICON_KEY, logo);
      meta.delete(LEGACY_LOGO_KEY);
    }
  }

  get name() {
    return this.#meta.get(NAME_KEY) || DEFAULT_NAME;
  }

  /**
   * Rename the server. Empty puts it back to the default.
   *
   * Invisible characters are stripped for the same reason they are in a
   * channel name: this ends up in a title bar beside things people trust,
   * and a right-to-left override there reverses whatever is next to it.
   */
  setName(raw) {
    const clean = String(raw ?? '')
      .replace(/[\p{Cc}\p{Cf}\p{Cs}]/gu, '')
      .replace(/\s+/g, ' ')
      .trim()
      .slice(0, MAX_NAME);
    if (clean) this.#meta.set(NAME_KEY, clean);
    else this.#meta.delete(NAME_KEY);
    return this.name;
  }

  /** The upload hash of the server's picture, or null for none. */
  get iconHash() {
    return this.#meta.get(ICON_KEY) || null;
  }

  /**
   * Point the server at a picture, or at none. Whether the hash names an
   * image the server holds is the caller's to check, as it is for avatars:
   * the uploads live in another module.
   *
   * @returns {string|null} the picture it had before, for releasing
   */
  setIcon(hash) {
    const previous = this.iconHash;
    if (hash) this.#meta.set(ICON_KEY, hash);
    else this.#meta.delete(ICON_KEY);
    return previous;
  }

  /**
   * The largest one file may be, or null when nobody has said: the default,
   * which is chat.js's to know. The range is the caller's to check, as the
   * picture's hash is.
   */
  get maxFileBytes() {
    const value = Number(this.#meta.get(MAX_FILE_KEY));
    return Number.isFinite(value) && value > 0 ? value : null;
  }

  setMaxFileBytes(bytes) {
    if (bytes === null) this.#meta.delete(MAX_FILE_KEY);
    else this.#meta.set(MAX_FILE_KEY, String(Math.round(bytes)));
  }

  /** The override, or null when the environment is still in charge. */
  #stored() {
    return this.#meta.get(DOOR_KEY);
  }

  /**
   * Is a password wanted at all?
   *
   * An override of the empty string is a real answer -- "the owner turned
   * the door off" -- and is deliberately different from no override, which
   * means "nobody has said, use the environment".
   */
  get passwordRequired() {
    const stored = this.#stored();
    if (stored === null) return Boolean(this.#envPassword);
    return stored !== '';
  }

  /**
   * True when the media relay's stance no longer matches this one.
   *
   * MediaMTX's open-server shortcut governs the legacy flat stream namespace:
   * with no `config.mediaToken` any read there is let through. Voice
   * channels are unaffected: their paths are checked before that shortcut is
   * ever reached, which is the security-critical ordering in the auth hook.
   *
   * The token used to be minted at boot or not at all, so turning the
   * password on from the client left that one legacy branch open until a
   * restart. index.js now keeps it in step with every change of the door,
   * which leaves this false; it stays, and keeps measuring, so that saying
   * so remains better than hoping nobody notices if the two ever part again.
   */
  get restartRequired() {
    return this.#relayGuarded() !== this.passwordRequired;
  }

  /** Does this attempt open the door? */
  matches(offered) {
    const stored = this.#stored();
    if (stored === null) return secretsMatch(offered, this.#envPassword);
    if (stored === '') return true;

    const [salt, expected] = stored.split(':');
    if (!salt || !expected) return false;
    const a = Buffer.from(hashDoor(String(offered ?? ''), salt), 'hex');
    const b = Buffer.from(expected, 'hex');
    // Equal length by construction -- both are a SHA-256 digest -- so there
    // is no length oracle to worry about here.
    return a.length === b.length && timingSafeEqual(a, b);
  }

  /**
   * Set the door password. An empty string takes the door off its hinges.
   *
   * There is no way back to "use the environment" from here, on purpose: a
   * control that sometimes means "clear this" and sometimes means "fall
   * back to whatever the host is configured with" is a control nobody can
   * predict. Once the owner has decided, the owner decides.
   */
  setPassword(raw) {
    const value = String(raw ?? '');
    if (!value) {
      this.#meta.set(DOOR_KEY, '');
      return { passwordRequired: false, restartRequired: this.restartRequired };
    }
    const salt = randomBytes(16).toString('hex');
    this.#meta.set(DOOR_KEY, `${salt}:${hashDoor(value, salt)}`);
    return { passwordRequired: true, restartRequired: this.restartRequired };
  }

  /**
   * The host changed HARMONY_PASSWORD and wants it live now, not at the next
   * restart. Empty means no password.
   *
   * It puts the environment back in charge, dropping any override the owner
   * set from the client: the host has just written this value into the env
   * file, and a stored override would keep winning over that file after
   * every restart from here on -- the env file saying one thing and the
   * server doing another, which is the state nobody can debug.
   */
  useEnvironmentPassword(raw) {
    this.#envPassword = String(raw ?? '');
    this.#meta.delete(DOOR_KEY);
    return { passwordRequired: this.passwordRequired };
  }

  /** Everything a client is allowed to know. */
  publicView() {
    return {
      name: this.name,
      // The picture's hash, under the name the Electron, browser and Android
      // clients know. Never also as `iconHash`: native clients from 4.0.8
      // take `logo` for `iconHash` and refuse a reply with both, and those
      // before 4.0.8 only go without the picture until they update.
      logo: this.iconHash,
      passwordRequired: this.passwordRequired,
      restartRequired: this.restartRequired,
    };
  }
}
