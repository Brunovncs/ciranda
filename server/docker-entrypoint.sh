#!/usr/bin/env bash
# Start the control server, then MediaMTX, and tie their lifetimes together.
#
# Order matters. MediaMTX asks the control server to authorise every publish and
# every read, so if it comes up first the requests that arrive in that window are
# refused rather than queued -- install.sh says the same thing about the systemd
# units. So: start the control server, wait until it actually answers, then bring
# up the media server.
#
# The container must also die when either process dies, not limp along serving
# half a service. A container that answers its health check while nobody can
# watch anything is worse than one that restarts.

set -euo pipefail

HARMONY_PORT="${HARMONY_PORT:-8080}"
MEDIAMTX_CONFIG="${MEDIAMTX_CONFIG:-/etc/harmony/mediamtx.yml}"
STARTUP_TIMEOUT="${HARMONY_STARTUP_TIMEOUT:-30}"
WATCHDOG_INTERVAL="${HARMONY_WATCHDOG_INTERVAL:-30}"
WATCHDOG_FAILURES="${HARMONY_WATCHDOG_FAILURES:-3}"

log() { printf '[entrypoint] %s\n' "$*"; }

if [ -z "${MTX_WEBRTCADDITIONALHOSTS:-}" ]; then
  log 'WARNING: MTX_WEBRTCADDITIONALHOSTS is not set.'
  log '         MediaMTX will only advertise addresses it finds on this'
  log "         container's interfaces, which are not reachable from outside."
  log '         Set it to the public hostname or IP clients will use.'
fi

# Cirandinha's yt-dlp. The image's copy is as old as the image, and YouTube
# breaks old versions within weeks, so the server runs a copy kept in the data
# volume (the root filesystem is read-only) that updates itself: seeded from
# the image the first time, then `yt-dlp -U` a minute after start and daily
# after that (below). -U checks what it downloads against the release's
# SHA2-256SUMS. Best effort and quiet -- nothing else depends on it, and with no
# volume copy the image's is used. HARMONY_YTDLP_AUTOUPDATE=0 keeps the image's;
# a HARMONY_YTDLP_BIN of your own is left alone. A volume copy that cannot run
# is replaced by the image's: an image that switched builds (the standalone one
# could not run here; see the Dockerfile) must not keep using the old one.
ytdlp_volume=''
if [ "${HARMONY_YTDLP_AUTOUPDATE:-1}" != 0 ] && [ -z "${HARMONY_YTDLP_BIN:-}" ] \
    && [ -x /usr/local/bin/yt-dlp ] && [ -n "${HARMONY_DATA_DIR:-}" ] \
    && mkdir -p "$HARMONY_DATA_DIR/bin" 2>/dev/null; then
  if [ ! -x "$HARMONY_DATA_DIR/bin/yt-dlp" ] \
      || ! timeout 60 "$HARMONY_DATA_DIR/bin/yt-dlp" --version >/dev/null 2>&1; then
    { cp /usr/local/bin/yt-dlp "$HARMONY_DATA_DIR/bin/yt-dlp.new" \
        && chmod 0755 "$HARMONY_DATA_DIR/bin/yt-dlp.new" \
        && mv -f "$HARMONY_DATA_DIR/bin/yt-dlp.new" "$HARMONY_DATA_DIR/bin/yt-dlp"; } 2>/dev/null \
      || rm -f "$HARMONY_DATA_DIR/bin/yt-dlp.new" 2>/dev/null || true
  fi
  if [ -x "$HARMONY_DATA_DIR/bin/yt-dlp" ]; then
    ytdlp_volume="$HARMONY_DATA_DIR/bin/yt-dlp"
    export HARMONY_YTDLP_BIN="$ytdlp_volume"
  fi
fi

node /opt/harmony/src/index.js &
control_pid=$!

# Wait for it to answer rather than sleeping a fixed amount: on a loaded
# Raspberry Pi the difference between "started" and "listening" is real.
deadline=$(( SECONDS + STARTUP_TIMEOUT ))
until wget -qO- "http://127.0.0.1:${HARMONY_PORT}/api/health" >/dev/null 2>&1; do
  if ! kill -0 "$control_pid" 2>/dev/null; then
    log 'control server exited during startup'
    wait "$control_pid"
    exit 1
  fi
  if [ "$SECONDS" -ge "$deadline" ]; then
    log "control server did not answer within ${STARTUP_TIMEOUT}s"
    kill -TERM "$control_pid" 2>/dev/null || true
    exit 1
  fi
  sleep 0.5
done
log "control server is answering on :${HARMONY_PORT}"

mediamtx "$MEDIAMTX_CONFIG" &
mediamtx_pid=$!

# The image's HEALTHCHECK only labels the container unhealthy, and nothing acts
# on that label: plain compose and `restart: unless-stopped` react to an exit,
# never to it. A server still running but no longer answering -- hung, or no
# longer seeing MediaMTX -- would stay up and useless until someone noticed. So
# this asks the HEALTHCHECK's question itself and, after WATCHDOG_FAILURES
# wrong answers in a row, exits, which takes the container down below and lets
# the restart policy bring it back. The first probe waits a full interval after
# MediaMTX starts, and it takes several failures in a row, so a slow start
# (MediaMTX reported "down" until the control server's first poll) is never
# mistaken for a hang. HARMONY_WATCHDOG_INTERVAL=0 turns it off.
watchdog() {
  local failures=0
  while :; do
    sleep "$WATCHDOG_INTERVAL"
    if wget -q -T 5 -O- "http://127.0.0.1:${HARMONY_PORT}/api/health" 2>/dev/null | grep -q '"mediamtx":"up"'; then
      failures=0
      continue
    fi
    failures=$(( failures + 1 ))
    log "health check failed (${failures} of ${WATCHDOG_FAILURES})"
    if [ "$failures" -ge "$WATCHDOG_FAILURES" ]; then
      log 'unhealthy for too long -- exiting so the restart policy starts it again'
      exit 3
    fi
  done
}
watchdog_pid=''
if [ "$WATCHDOG_INTERVAL" -gt 0 ]; then
  watchdog &
  watchdog_pid=$!
fi

# The volume's yt-dlp, kept current (see above). After the start rather than
# before it, so a slow or unreachable GitHub never holds the server up. Not
# waited on below: an update that fails is a line in the log, not a reason to
# stop the container. The server picks a new version up at its next yt-dlp run.
ytdlp_updater() {
  sleep 60
  while :; do
    local out
    if out="$("$ytdlp_volume" -U 2>&1)"; then
      log "yt-dlp: $(printf '%s\n' "$out" | tail -n1)"
    else
      log "yt-dlp could not update itself: $(printf '%s\n' "$out" | tail -n1)"
    fi
    sleep 86400
  done
}
ytdlp_pid=''
if [ -n "$ytdlp_volume" ]; then
  ytdlp_updater &
  ytdlp_pid=$!
fi

shutdown() {
  trap - TERM INT
  log 'shutting down'
  # shellcheck disable=SC2086 # empty when the watchdog is off, and must then vanish
  kill -TERM $watchdog_pid $ytdlp_pid "$mediamtx_pid" "$control_pid" 2>/dev/null || true
  wait "$mediamtx_pid" "$control_pid" 2>/dev/null || true
  exit 0
}
trap shutdown TERM INT

# Whichever exits first takes the container with it, the watchdog included.
# `|| status=$?`, not `status=$?` on the next line: under `set -e` a non-zero
# exit would end the script right at the `wait`, leaving the other process to be
# killed without a word in the log.
status=0
# shellcheck disable=SC2086
wait -n "$control_pid" "$mediamtx_pid" $watchdog_pid || status=$?

if [ -n "$watchdog_pid" ] && ! kill -0 "$watchdog_pid" 2>/dev/null; then
  log "the health watchdog gave up (status ${status}) -- stopping both"
elif kill -0 "$control_pid" 2>/dev/null; then
  log "mediamtx exited (status ${status}) -- stopping the control server"
else
  log "the control server exited (status ${status}) -- stopping mediamtx"
fi
# shellcheck disable=SC2086
kill -TERM $watchdog_pid $ytdlp_pid "$mediamtx_pid" "$control_pid" 2>/dev/null || true
wait 2>/dev/null || true

# Never exit 0 here. Reaching this point means a process ended by itself rather
# than because the container was asked to stop -- and MediaMTX exits 0 on a
# clean shutdown, so passing its status through would report a crash-out as
# success and stop `restart: on-failure` from ever restarting it.
exit "$(( status == 0 ? 1 : status ))"
