#!/usr/bin/env python3
"""Spotify on the station through alice-server.

librespot (https://github.com/librespot-org/librespot) runs as a Spotify
Connect speaker. Its PCM is encoded to MP3 by ffmpeg and served at
/stream.mp3, which the station plays with audio_play: when Spotify starts
playing on the speaker the station is told to play the stream, when Spotify
pauses or moves to another device the station pauses (push-directive of
alice-server's management API). While the station does not read the stream
(its own pause), librespot is held back until it reads again. The sender's
volume moves the station's own volume, which changes at once: in the stream a
change would be heard only after what the station has buffered.

The first start needs a Spotify sign-in: the page at / shows the link and
takes the address Spotify redirects to afterwards. With SPOTIFY_CLIENT_ID and
SPOTIFY_CLIENT_SECRET of an app on developer.spotify.com the assistant also
gets voice control: search and play, pause, next and previous track (the
function server protocol of alice-server at /functions, the playback state at
/state).

With AIRPLAY_FIFO_PATH the speaker takes AirPlay as well: shairport-sync
(docker/airplay) writes the sound it receives to that named pipe, which goes
out in the same stream. Of Spotify and AirPlay, the last to start has the
station.

librespot runs this file as its --onevent program too: with PLAYER_EVENT in
the environment it only hands the event over to the running bridge.
"""
import os
import sys

PORT = int(os.environ.get("PORT") or 8090)
# what librespot reports through --onevent
EVENT_FIELDS = ("PLAYER_EVENT", "TRACK_ID", "URI", "NAME", "ARTISTS", "ALBUM", "COVERS", "DURATION_MS", "POSITION_MS", "VOLUME", "CLIENT_NAME")


def hand_over_event():
    """The --onevent mode: the event goes to the bridge, whatever happens to
    it. librespot waits for the program on every event, so it imports next to
    nothing (urllib alone doubles its time) and waits for the bridge's answer,
    which keeps the events in order"""
    import json
    import socket
    body = json.dumps({name: os.environ[name] for name in EVENT_FIELDS if name in os.environ}).encode()
    try:
        with socket.create_connection(("127.0.0.1", PORT), timeout=5) as connection:
            connection.sendall(b"POST /event HTTP/1.0\r\nContent-Type: application/json\r\nContent-Length: %d\r\n\r\n%s"
                               % (len(body), body))
            connection.recv(1024)
    except OSError:
        pass


if __name__ == "__main__" and "PLAYER_EVENT" in os.environ:
    hand_over_event()
    sys.exit(0)

# the bridge itself: the --onevent mode above does without all this
import base64
import collections
import hashlib
import html
import re
import http.server
import json
import logging
import secrets
import select
import signal
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path


def env(name, default=None):
    return (os.environ.get(name) or "").strip() or default


NAME = env("SPOTIFY_NAME", "Алиса")
STREAM_URL = env("STREAM_URL")
ALICE_API_URL = (env("ALICE_API_URL") or "").rstrip("/")
ALICE_API_TOKEN = env("ALICE_API_TOKEN")
STATION_ID = env("STATION_ID", "all")
PROXY = env("SPOTIFY_PROXY")
CLIENT_ID = env("SPOTIFY_CLIENT_ID")
CLIENT_SECRET = env("SPOTIFY_CLIENT_SECRET")
REDIRECT_URI = env("SPOTIFY_REDIRECT_URI", "http://127.0.0.1:8888/callback")
BITRATE = env("MP3_BITRATE", "256k")
LIBRESPOT = env("LIBRESPOT_BIN", "librespot")
DATA = Path(env("DATA_DIR", "/data"))
CACHE = DATA / "librespot"
CREDENTIALS = CACHE / "credentials.json"
TOKENS = DATA / "spotify-api.json"
FIFO = Path(env("FIFO_PATH", "/tmp/librespot.pcm"))
AIRPLAY_FIFO = env("AIRPLAY_FIFO_PATH")

RATE = 44100 * 2 * 2  # bytes per second of 16-bit stereo PCM at 44.1 kHz
CHUNK = RATE // 20  # 50 ms
# how far the stream may run ahead of real time, so the station starts with a buffer
LEAD = float(env("LEAD_MS", "2000")) / 1000
# a station that takes no data for this long has paused its player
STALL = float(env("STALL_MS", "2000")) / 1000
# with the Web API such a station has Spotify paused, and librespot holds back
# only after this long: blocked in its write it would not take that pause
HOLD_AFTER = 5
# librespot's log lines for a lost connection to Spotify. It may not come back by
# itself: with nothing to wake it, it sits on the dead session for days, gone from
# the user's devices (librespot-org/librespot#1692)
CONNECTION_LOST = ("Connection to server closed", "Websocket peer does not respond")
# AirPlay's sound that stops for this long was paused or disconnected
AIRPLAY_QUIET = 3
# a step of the station's volume for a tenth of Spotify's slider, and for each press of a
# Mac's volume key (AirPlay's -30 to 0 dB in 16 presses, the step a little under one)
SPOTIFY_VOLUME_STEP = 65535 / 10
AIRPLAY_VOLUME_STEP = 1.8
SCOPES = "user-read-playback-state user-modify-playback-state user-read-currently-playing playlist-read-private user-library-read user-library-modify"

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
log = logging.getLogger("spotify-bridge")


def http_json(method, url, body=None, headers=None, form=None, proxy=None, timeout=10):
    """A JSON request; None for an answer that is empty or not JSON, HTTPError raised for 4xx and 5xx"""
    data, all_headers = None, dict(headers or {})
    if form is not None:
        data = urllib.parse.urlencode(form).encode()
        all_headers["Content-Type"] = "application/x-www-form-urlencoded"
    elif body is not None:
        data = json.dumps(body).encode()
        all_headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, headers=all_headers, method=method)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({"http": proxy, "https": proxy} if proxy else {}))
    with opener.open(request, timeout=timeout) as response:
        data = response.read(2 * 1024 * 1024 + 1)
        if len(data) > 2 * 1024 * 1024:
            raise ValueError("JSON response too large")
        text = data.decode()
    try:
        return json.loads(text)
    except ValueError:
        # Spotify's player commands answer 200 with a body that is not JSON, not the documented 204
        return None


def struct(value):
    """A JSON object as a protobufjs google.protobuf.Struct (push-directive's Payload)"""
    def wrap(item):
        if isinstance(item, bool):
            return {"boolValue": item}
        if isinstance(item, (int, float)):
            return {"numberValue": item}
        if isinstance(item, str):
            return {"stringValue": item}
        if isinstance(item, dict):
            return {"structValue": struct(item)}
        if isinstance(item, list):
            return {"listValue": {"values": [wrap(each) for each in item]}}
        return {"nullValue": 0}
    return {"fields": [{"key": key, "value": wrap(item)} for key, item in value.items()]}


def file_version(path):
    """What tells one write of a file from the next; None while there is no file"""
    try:
        stat = path.stat()
    except OSError:
        return None
    return stat.st_ino, stat.st_mtime_ns


def album_cover(covers):
    """librespot emits cover URLs largest first, without their dimensions.
    Prefer the medium variant when available for the compact player card."""
    urls = []
    for value in covers.splitlines():
        value = value.strip()
        if not value or "\\" in value or any(character.isspace() for character in value):
            continue
        try:
            parsed = urllib.parse.urlsplit(value)
            if (parsed.scheme == "https" and parsed.hostname and parsed.username is None
                    and parsed.password is None):
                # Accessing port also rejects an invalid or out-of-range port.
                parsed.port
                if value not in urls:
                    urls.append(value)
        except ValueError:
            continue
    return urls[1] if len(urls) >= 3 else next(iter(urls), None)


class CoverCache:
    """Only event-selected covers can be fetched; downloads never hold the playback lock."""

    MAX_BYTES = 2 * 1024 * 1024
    TYPES = {"image/jpeg", "image/png", "image/webp"}

    def __init__(self):
        self.entries = collections.OrderedDict()
        self.lock = threading.Lock()

    def remember(self, url):
        if not url:
            return None
        key = hashlib.sha256(url.encode()).hexdigest()
        with self.lock:
            if key not in self.entries:
                self.entries[key] = {"url": url, "image": None, "lock": threading.Lock(), "retry_after": 0}
            self.entries.move_to_end(key)
            while len(self.entries) > 8:
                self.entries.popitem(last=False)
        return key

    def get(self, key):
        with self.lock:
            entry = self.entries.get(key)
        if entry is None:
            raise KeyError(key)
        if not entry["lock"].acquire(timeout=8):
            raise TimeoutError("cover download busy")
        try:
            if entry["image"] is None:
                if time.monotonic() < entry["retry_after"]:
                    raise OSError("cover temporarily unavailable")
                try:
                    entry["image"] = self.fetch(entry["url"])
                except (OSError, ValueError):
                    entry["retry_after"] = time.monotonic() + 30
                    raise
            return entry["image"]
        finally:
            entry["lock"].release()

    @classmethod
    def fetch(cls, url):
        parsed = urllib.parse.urlsplit(url)
        image_id = parsed.path.removeprefix("/image/")
        urls = [url]
        if (parsed.netloc == "i.scdn.co" and parsed.path.startswith("/image/")
                and len(image_id) == 40 and all(character in "0123456789abcdef" for character in image_id)
                and not parsed.query and not parsed.fragment):
            # The same Spotify image is available on these CDNs when i.scdn.co
            # is unreachable. Keep the cache key tied to the original event URL.
            urls = [f"https://image-cdn-{edge}.spotifycdn.com/image/{image_id}" for edge in ("ak", "fa")]
        deadline = time.monotonic() + 6
        for candidate in urls:
            try:
                return cls.download(candidate, deadline)
            except (OSError, ValueError):
                if time.monotonic() >= deadline:
                    break
        raise OSError("cover download failed")

    @classmethod
    def download(cls, url, deadline):
        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, request, response, code, message, headers, new_url):
                raise ValueError("cover redirects are not supported")

        opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({"http": PROXY, "https": PROXY} if PROXY else {}), NoRedirect())
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("cover download timed out")
        with opener.open(url, timeout=min(3, remaining)) as response:
            content_type = response.headers.get_content_type()
            if content_type not in cls.TYPES:
                raise ValueError("unsupported cover type")
            if int(response.headers.get("Content-Length") or 0) > cls.MAX_BYTES:
                raise ValueError("cover too large")
            body = bytearray()
            while True:
                if time.monotonic() > deadline:
                    raise TimeoutError("cover download timed out")
                chunk = response.read1(min(65536, cls.MAX_BYTES + 1 - len(body)))
                if not chunk:
                    break
                body.extend(chunk)
                if len(body) > cls.MAX_BYTES:
                    raise ValueError("cover too large")
        valid = ((content_type == "image/jpeg" and body.startswith(b"\xff\xd8\xff"))
                 or (content_type == "image/png" and body.startswith(b"\x89PNG\r\n\x1a\n"))
                 or (content_type == "image/webp" and body[:4] == b"RIFF" and body[8:12] == b"WEBP"))
        if not valid:
            raise ValueError("invalid cover image")
        return content_type, bytes(body)


class Station:
    """The station, through the management API of alice-server"""

    def __init__(self):
        # what the station was told last: a paused station still connects to the stream to fill its buffer
        self.playing = False
        self.stream_id = None

    def push(self, name, payload, led_silent):
        if not ALICE_API_URL:
            log.warning("ALICE_API_URL is not set: cannot tell the station to %s", name)
            return
        directive = {"Name": name, "Type": "client_action", "Payload": struct(payload), "IsLedSilent": led_silent}
        try:
            headers = {"Authorization": f"Bearer {ALICE_API_TOKEN}"} if ALICE_API_TOKEN else None
            http_json("POST", f"{ALICE_API_URL}/devices/{urllib.parse.quote(STATION_ID)}/push-directive", body=directive, headers=headers)
            log.info("station %s: %s", STATION_ID, name)
        except (OSError, ValueError) as error:
            log.warning("pushing %s to the station failed: %s", name, error)

    def play(self):
        url = f"{STREAM_URL}{'&' if '?' in STREAM_URL else '?'}session={uuid.uuid4().hex}"
        self.stream_id = str(uuid.uuid4())
        stream = {"format": "MP3", "id": self.stream_id, "type": "Track", "url": url}
        self.playing = True
        self.push("audio_play", {"background_mode": "Ducking", "callbacks": {}, "screen_type": "Music", "stream": stream}, False)

    def pause(self):
        self.playing = False
        # player_pause is taken only with the next directive: the station played out what it had
        # buffered first, two seconds and more. audio_stop stops it at once, and play() starts
        # a new stream anyway
        self.push("audio_stop", {}, True)

    def change_volume(self, steps):
        for _ in range(abs(steps)):
            self.push("sound_louder" if steps > 0 else "sound_quiter", {}, True)


class VolumeSteps:
    """A sender's volume as steps of the station's, from where the sender was when first
    heard: it does not know the station's volume, which would jump to the sender's"""

    def __init__(self, step):
        self.step = step
        self.last = None

    def follow(self, volume):
        """The station's steps to this volume, louder above zero"""
        if self.last is None:
            self.last = volume
            return 0
        steps = int((volume - self.last) / self.step)
        self.last += steps * self.step
        return steps

    def forget(self):
        """The next volume is where the sender starts again"""
        self.last = None


class Listener:
    """The station's connection to /stream.mp3"""

    def __init__(self, address):
        self.address = address
        self.chunks = collections.deque()
        self.closed = False
        self.stalled = False
        self.stalled_at = None


class Bridge:
    def __init__(self):
        self.lock = threading.Condition()
        self.station = Station()
        self.listener = None
        self.state = "stopped"
        # librespot's state events so far, to wait for the next one
        self.state_changes = 0
        # the last track played to its end / the stream jumped since (a skip, a seek)
        self.played_out = False
        self.jumped = False
        self.track = None
        self.covers = CoverCache()
        self.last_play_push = None
        self.librespot_status = "starting"
        self.sign_in_url = None
        self.sign_in_answer = None
        self.events = collections.deque(maxlen=20)
        self.api = SpotifyApi() if CLIENT_ID and CLIENT_SECRET else None
        self.encoder = None
        # AirPlay's sound: None while there is none, "playing" on the station, or "ignored" after Spotify took it
        self.airplay = None
        self.spotify_volume = VolumeSteps(SPOTIFY_VOLUME_STEP)
        self.airplay_volume = VolumeSteps(AIRPLAY_VOLUME_STEP)
        self.music = MusicService(self)

    # librespot

    def run_librespot(self):
        backoff = 2
        while True:
            CACHE.mkdir(parents=True, exist_ok=True)
            started = time.monotonic()
            try:
                if CREDENTIALS.is_file():
                    self.play_librespot()
                else:
                    self.sign_in()
            except OSError as error:
                with self.lock:
                    self.librespot_status = f"не запускается: {error}"
                log.error("librespot: %s", error)
            if time.monotonic() - started > 60:
                backoff = 2
            time.sleep(backoff)
            backoff = min(backoff * 2, 60)

    def librespot_command(self, *arguments):
        command = [LIBRESPOT, "--name", NAME, "--device-type", "speaker", "--cache", str(CACHE), "--system-cache", str(CACHE),
                   "--disable-audio-cache", "--disable-discovery", *arguments]
        return command + (["--proxy", PROXY] if PROXY else [])

    def sign_in(self):
        """librespot's OAuth sign-in by hand: it prints the link and reads the
        address Spotify redirected to; the credentials stay in the cache"""
        # port 0: no redirect listener, librespot reads the redirected address from stdin
        command = self.librespot_command("--enable-oauth", "--oauth-port", "0", "--backend", "pipe", "--device", "/dev/null")
        process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
        with self.lock:
            self.librespot_status = "нужен вход в Spotify"
            self.sign_in_answer = None
        threading.Thread(target=self.read_sign_in_output, args=(process,), daemon=True).start()
        try:
            while process.poll() is None and not CREDENTIALS.is_file():
                with self.lock:
                    answer, self.sign_in_answer = self.sign_in_answer, None
                    if answer is None:
                        self.lock.wait(1)
                        continue
                process.stdin.write(answer.strip() + "\n")
                process.stdin.flush()
                with self.lock:
                    self.librespot_status = "вход…"
            if CREDENTIALS.is_file():
                log.info("signed in to Spotify")
        finally:
            with self.lock:
                self.sign_in_url = None
            process.terminate()
            process.wait()

    def read_sign_in_output(self, process):
        for line in process.stdout:
            line = line.rstrip()
            if line.startswith("Browse to: "):
                with self.lock:
                    self.sign_in_url = line.removeprefix("Browse to: ")
                    self.lock.notify_all()
                log.info("librespot waits for a sign-in on the page at /")
            elif line:
                log.info("librespot: %s", line)

    def play_librespot(self):
        # the volume goes to the station's (on_event), so the stream keeps its level: 0.1 dB is about
        # the least range librespot's cubic curve takes, at 0 it is linear, and fixed hides the slider
        command = self.librespot_command("--backend", "pipe", "--device", str(FIFO), "--format", "S16", "--bitrate", "320",
                                         "--initial-volume", "100", "--volume-ctrl", "cubic", "--volume-range", "0.1",
                                         "--onevent", str(Path(__file__).resolve()))
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
        with self.lock:
            self.librespot_status = "работает"
        for line in process.stdout:
            line = line.rstrip()
            if line:
                log.info("librespot: %s", line)
            if "Bad credentials" in line or "BadCredentials" in line:
                log.warning("Spotify refused the saved credentials: signing in again")
                CREDENTIALS.unlink(missing_ok=True)
            if any(message in line for message in CONNECTION_LOST):
                log.warning("librespot lost its connection: restarting it")
                process.terminate()
        code = process.wait()
        with self.lock:
            self.librespot_status = f"перезапуск (код {code})"
            playing = self.state == "playing"
        log.warning("librespot exited with %s", code)
        if playing:
            # no event comes from a librespot that is gone
            self.on_event({"PLAYER_EVENT": "session_disconnected"})

    # sound

    def run_encoder(self):
        while True:
            # no analysis of the input: it would hold back the first 4.6 s of sound that comes in real time
            command = ["ffmpeg", "-loglevel", "error", "-probesize", "32", "-analyzeduration", "0",
                       "-f", "s16le", "-ar", "44100", "-ac", "2", "-i", "pipe:0",
                       "-c:a", "libmp3lame", "-b:a", BITRATE, "-f", "mp3", "-write_xing", "0", "-id3v2_version", "0",
                       "-flush_packets", "1", "pipe:1"]
            process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
            with self.lock:
                self.encoder = process
                self.lock.notify_all()
            while chunk := process.stdout.read1(16384):
                with self.lock:
                    if self.listener and not self.listener.closed:
                        self.listener.chunks.append(chunk)
                        self.lock.notify_all()
            log.warning("the MP3 encoder exited with %s", process.wait())
            time.sleep(1)

    def run_pump(self):
        """librespot's PCM into the encoder at the pace of real time, a little
        ahead; held back (and with it librespot) while the station is not
        reading. librespot closes the pipe when it pauses or stops"""
        if FIFO.exists() and not FIFO.is_fifo():
            FIFO.unlink()
        if not FIFO.exists():
            os.mkfifo(FIFO)
        while True:
            with open(FIFO, "rb", buffering=0) as pipe:
                started, sent, listener = time.monotonic(), 0, None
                while True:
                    current, waited = self.wait_for_listener()
                    started += waited
                    if current is not listener:
                        # a new connection gets its lead again, so that the station starts at once
                        started, sent, listener = time.monotonic(), 0, current
                    data = pipe.read(CHUNK)
                    if not data:
                        break
                    ahead = sent / RATE - (time.monotonic() - started) - LEAD
                    if ahead > 0:
                        time.sleep(ahead)
                    if not self.encode(data):
                        break
                    sent += len(data)

    def wait_for_listener(self):
        """The station that reads the stream, and the seconds spent waiting for it"""
        started = time.monotonic()
        with self.lock:
            while not self.taking(self.listener):
                self.lock.wait(1)
            listener = self.listener
        return listener, time.monotonic() - started

    def taking(self, listener):
        """Whether librespot's sound goes on to this listener (under the lock)"""
        if not listener or listener.closed or self.airplay == "playing":
            return False
        return not listener.stalled or (self.api is not None and time.monotonic() - listener.stalled_at < HOLD_AFTER)

    def encode(self, data):
        with self.lock:
            while self.encoder is None or self.encoder.poll() is not None:
                self.lock.wait(1)
            encoder = self.encoder
        try:
            encoder.stdin.write(data)
            encoder.stdin.flush()
            return True
        except (AttributeError, OSError):
            return False

    def serve_listener(self, handler):
        """The station's stream: MP3 as it is encoded; a newer connection
        replaces this one. A station that takes no data for STALL seconds has
        paused: librespot is held back, with the Web API only after HOLD_AFTER
        seconds since Spotify pauses then"""
        listener = Listener(handler.client_address[0])
        with self.lock:
            if self.listener:
                self.listener.closed = True
            self.listener = listener
            self.lock.notify_all()
        log.info("the station %s listens", listener.address)
        connection = handler.connection
        connection.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 65536)
        connection.settimeout(0.25)
        blocked_since = None
        try:
            while True:
                with self.lock:
                    while not listener.chunks and not listener.closed:
                        self.lock.wait(1)
                    if listener.closed:
                        return
                    chunk = listener.chunks.popleft()
                view = memoryview(chunk)
                while view:
                    try:
                        sent = connection.send(view)
                        view = view[sent:]
                        blocked_since = None
                        if listener.stalled:
                            self.station_resumed(listener)
                    except TimeoutError:
                        blocked_since = blocked_since or time.monotonic()
                        if not listener.stalled and time.monotonic() - blocked_since > STALL:
                            self.station_paused(listener)
                    with self.lock:
                        if listener.closed:
                            return
        except OSError as error:
            log.info("the station %s stopped listening: %s", listener.address, type(error).__name__)
        finally:
            with self.lock:
                listener.closed = True
                if self.listener is listener:
                    self.listener = None
                self.lock.notify_all()

    def station_paused(self, listener):
        with self.lock:
            listener.stalled = True
            listener.stalled_at = time.monotonic()
            playing = self.state == "playing"
        log.info("the station paused the stream")
        if playing and self.api:
            self.api.try_call("pause")

    def station_resumed(self, listener):
        with self.lock:
            listener.stalled = False
            self.lock.notify_all()
            paused = self.state != "playing" and self.airplay != "playing"
        log.info("the station reads the stream again")
        if paused and self.api:
            self.api.try_call("resume")

    def listening(self):
        """Whether the station reads the stream now"""
        with self.lock:
            return bool(self.listener and not self.listener.closed and not self.listener.stalled)

    # AirPlay

    def run_airplay(self):
        """shairport-sync's PCM into the encoder. It comes at the pace of real
        time and cannot be held back: what the station does not read is dropped.
        Opened for writing too, so that a restart of shairport-sync is no end of file"""
        try:
            os.mkfifo(AIRPLAY_FIFO)
        except FileExistsError:
            pass
        pipe = os.open(AIRPLAY_FIFO, os.O_RDWR)
        while True:
            if select.select([pipe], [], [], AIRPLAY_QUIET)[0]:
                self.airplay_sound(os.read(pipe, CHUNK))
            else:
                self.airplay_quiet()

    def airplay_sound(self, data):
        with self.lock:
            started = self.airplay is None
            if started:
                self.airplay = "playing"
                self.events.append(f"{time.strftime('%H:%M:%S')} AirPlay")
            playing, spotify = self.airplay == "playing", self.state == "playing"
        if started:
            log.info("AirPlay started")
            # the pipe has to be read on in the meantime
            threading.Thread(target=self.airplay_take_over, args=(spotify,), daemon=True).start()
        if playing and self.listening():
            self.encode(data)

    def airplay_take_over(self, spotify):
        self.station.play()
        if spotify and self.api:
            self.api.try_call("pause")

    def airplay_quiet(self):
        with self.lock:
            if self.airplay is None:
                return
            playing, self.airplay = self.airplay == "playing", None
            # macOS starts each session at a third of its slider
            self.airplay_volume.forget()
            self.lock.notify_all()
        log.info("AirPlay stopped")
        if playing:
            self.station.pause()

    def run_airplay_volume(self):
        """The sender's volume, which docker/airplay/airplay-volume writes next to the pipe on
        each change: -30 to 0 dB, -144 for mute"""
        path = Path(AIRPLAY_FIFO).with_name("volume")
        # one written before the bridge started is no change
        seen = file_version(path)
        while True:
            time.sleep(0.1)
            version = file_version(path)
            if version == seen:
                continue
            seen = version
            try:
                volume = float(path.read_text())
            except (OSError, ValueError):
                continue
            # mute and anything else leave the station's volume as it is
            if not -30 <= volume <= 0:
                continue
            with self.lock:
                steps = self.airplay_volume.follow(volume)
            if steps:
                self.station.change_volume(steps)

    # librespot's events

    def on_event(self, fields):
        event = fields.get("PLAYER_EVENT", "")
        if event in ("preloading", "preload_next", "play_request_id_changed", "sink"):
            return
        with self.lock:
            if event != "position_correction":
                self.events.append(f"{time.strftime('%H:%M:%S')} {event}")
            if self.track and self.state == "playing" and event == "paused" and "POSITION_MS" not in fields:
                self.track["position_ms"] = (self.track.get("position_ms") or 0) + max(
                    0, int((time.monotonic() - self.track.get("position_at", time.monotonic())) * 1000))
                self.track["position_at"] = time.monotonic()
            if event == "track_changed":
                artists = ", ".join(filter(None, fields.get("ARTISTS", "").split("\n")))
                cover = album_cover(fields.get("COVERS", ""))
                self.track = {
                    "id": fields.get("TRACK_ID", ""), "name": fields.get("NAME", ""),
                    "artists": artists, "uri": fields.get("URI", ""),
                    "album": fields.get("ALBUM", "").strip() or None,
                    "cover_url": cover, "cover_key": self.covers.remember(cover),
                    "duration_ms": integer_ms(fields.get("DURATION_MS")),
                    "position_ms": 0, "position_at": time.monotonic(),
                }
            elif event == "playing":
                self.state = "playing"
                if self.airplay:
                    self.airplay = "ignored"
            elif event == "paused":
                self.state = "paused"
            elif event in ("stopped", "session_disconnected"):
                self.state = "stopped"
                self.track = None
            elif event == "end_of_track":
                self.played_out = True
            elif event == "loading":
                # a track that does not follow the end of the last one: a skip, another context
                self.jumped |= not self.played_out
                self.played_out = False
                if self.track and fields.get("TRACK_ID") != self.track.get("id"):
                    self.track = None
            elif event == "seeked":
                self.jumped = True
            elif event in ("session_connected", "session_client_changed"):
                # another controller, with a slider of its own
                self.spotify_volume.forget()
                if event == "session_connected":
                    self.track = None
            if self.track:
                position = integer_ms(fields.get("POSITION_MS"))
                if position is not None:
                    self.track["position_ms"] = position
                    self.track["position_at"] = time.monotonic()
            volume = fields.get("VOLUME", "") if event == "volume_changed" else ""
            volume_steps = self.spotify_volume.follow(int(volume)) if volume.isdigit() else 0
            if event in ("playing", "paused", "stopped", "session_disconnected"):
                self.state_changes += 1
            state, airplay = self.state, self.airplay == "playing"
            # what the station has buffered is stale after a jump: a new connection drops it
            restart = state == "playing" and event in ("playing", "seeked") and self.jumped
            if restart:
                self.jumped = False
            self.lock.notify_all()
        if event != "position_correction":
            log.info("librespot event %s", event)
        if volume_steps:
            self.station.change_volume(volume_steps)
        if state == "playing" and event in ("playing", "seeked") and (restart or not (self.station.playing and self.listening())):
            # the station plays the stream; a push that is still connecting is not repeated
            with self.lock:
                recent = (self.last_play_push is not None and time.monotonic() - self.last_play_push < 5
                          and self.listener is None)
                if not recent:
                    self.last_play_push = time.monotonic()
            if not recent:
                self.station.play()
        elif event in ("paused", "stopped", "session_disconnected") and state != "playing" and not airplay:
            self.station.pause()

    # the assistant's functions and state (alice-server's protocols)

    def catalog(self):
        functions = {
            "speaker_player_resume": {
                "description": "Resume the current Spotify or AirPlay stream on this speaker, without changing its source",
                "arguments": {},
                "hasResponse": True,
            },
            "spotify_play": {
                "description": "Play music on this speaker from Spotify (has response). Without a query it resumes what was "
                               "playing; with one it searches Spotify. Use it for any request to play or resume music.",
                "arguments": {
                    "query": {"type": "string", "description": "What to play: a track, an artist, an album or a playlist, as the user named it"},
                    "type": {"type": "string", "enum": ["track", "artist", "album", "playlist"], "description": "What the query names; track if unclear"},
                },
                "hasResponse": True,
            },
            # the station pauses with the answer itself, not only by a push in the middle of the dialog
            "spotify_pause": {"description": "Pause the Spotify music on this speaker; use it rather than smart_speaker_player_pause "
                                             "while Spotify plays", "arguments": {}, "directive": "smart_speaker_player_pause"},
        }
        if self.api:
            functions["spotify_next"] = {"description": "Next track in Spotify on this speaker", "arguments": {}}
            functions["spotify_previous"] = {"description": "Previous track in Spotify on this speaker", "arguments": {}}
        return functions

    def call(self, name, arguments):
        try:
            if name == "speaker_player_resume":
                return self.play(None, "track")
            if name == "spotify_play":
                return self.play(arguments.get("query"), arguments.get("type") or "track")
            if name == "spotify_pause":
                if self.api:
                    self.api.call("pause")
                else:
                    self.station.pause()
                return "ok"
            if name == "spotify_next":
                self.api.call("next")
                return "ok"
            if name == "spotify_previous":
                self.api.call("previous")
                return "ok"
            return f"error: no function {name}"
        except SpotifyError as error:
            return f"error: {error}"

    def play(self, query, kind):
        if not query:
            with self.lock:
                changes, playing, airplay = self.state_changes, self.state == "playing", self.airplay == "playing"
            if airplay or not self.api:
                self.station.play()
                return "ok: продолжаю"
            if playing:
                if not (self.station.playing and self.listening()):
                    self.station.play()
                return "ok: уже играет"
            self.api.call("take_over")
            # librespot comes out of a transfer paused, play: true notwithstanding
            if self.wait_state_change(changes, 5) != "playing":
                self.api.call("resume")
            return "ok: продолжаю"
        if not self.api:
            return "error: голосовой поиск в Spotify не подключён (нужны SPOTIFY_CLIENT_ID и SPOTIFY_CLIENT_SECRET)"
        found = self.api.find(query, kind)
        if not found:
            return f"error: в Spotify ничего не нашлось по запросу «{query}»"
        self.api.call("play", found["uri"], kind)
        return f"ok: включаю {found['title']}"

    def wait_state_change(self, changes, seconds):
        """librespot's state after its next change past `changes`, or as it is after `seconds`"""
        with self.lock:
            self.lock.wait_for(lambda: self.state_changes > changes, seconds)
            return self.state

    def now_playing(self):
        with self.lock:
            track, state, airplay = self.track, self.state, self.airplay == "playing"
        if airplay:
            return "На колонке играет звук по AirPlay"
        if not track or state == "stopped":
            return "В Spotify на колонке сейчас ничего не играет"
        title = f"«{track['name']}» — {track['artists']}" if track["artists"] else f"«{track['name']}»"
        return f"Сейчас играет {title}" if state == "playing" else f"На паузе {title}"

    def state_entries(self):
        with self.lock:
            airplay = self.airplay == "playing"
            state = "playing" if airplay else self.state
            if state == "playing" and self.listener and self.listener.stalled:
                state = "paused"
            track = self.track if not airplay and state != "stopped" else None
            player = {
                "duid": STATION_ID,
                "source": "AirPlay" if airplay else "Spotify",
                "state": state,
                "title": track.get("name") if track else None,
                "artist": track.get("artists") if track else None,
                "album": track.get("album") if track else None,
                "cover_url": track.get("cover_url") if track else None,
                "cover_key": track.get("cover_key") if track else None,
                "stream_id": self.station.stream_id,
            }
        return {
            "spotify": {"description": "Spotify music on this speaker", "value": self.now_playing()},
            "player": {"description": "Playback source and track on this speaker", "value": player},
        }


class SpotifyError(Exception):
    def __init__(self, message, status=None):
        super().__init__(message)
        self.status = status


class SpotifyApi:
    """The Web API of the user's own app on developer.spotify.com"""

    accounts = "https://accounts.spotify.com"
    api = "https://api.spotify.com/v1"

    def __init__(self):
        self.lock = threading.RLock()
        self.tokens = json.loads(TOKENS.read_text()) if TOKENS.is_file() else None
        self.state = secrets.token_urlsafe(16)
        self.device = None
        self.retry_after = 0

    def connected(self):
        return bool(self.tokens and self.tokens.get("refresh_token"))

    def authorize_url(self):
        query = {"client_id": CLIENT_ID, "response_type": "code", "redirect_uri": REDIRECT_URI, "scope": SCOPES, "state": self.state}
        return f"{self.accounts}/authorize?{urllib.parse.urlencode(query)}"

    def finish_authorization(self, redirected):
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(redirected.strip()).query)
        if query.get("state", [""])[0] != self.state:
            raise SpotifyError("ссылка не от этого входа: начните заново")
        if "code" not in query:
            raise SpotifyError(query.get("error", ["в адресе нет кода"])[0])
        self.save(self.token_request({"grant_type": "authorization_code", "code": query["code"][0], "redirect_uri": REDIRECT_URI}))

    def token_request(self, form):
        try:
            return http_json("POST", f"{self.accounts}/api/token", form=form, proxy=PROXY,
                             headers={"Authorization": "Basic " + base64.b64encode(f"{CLIENT_ID}:{CLIENT_SECRET}".encode()).decode()})
        except urllib.error.HTTPError as error:
            raise SpotifyError(f"Spotify отказал в токене: {error.code} {error.read().decode()[:200]}") from None
        except OSError as error:
            raise SpotifyError(f"Spotify недоступен: {error}") from None

    def save(self, answer):
        with self.lock:
            tokens = dict(self.tokens or {})
            tokens.update(answer)
            tokens["expires_at"] = time.time() + int(answer.get("expires_in", 3600)) - 60
            self.tokens = tokens
            TOKENS.parent.mkdir(parents=True, exist_ok=True)
            TOKENS.write_text(json.dumps(tokens))

    def access_token(self):
        with self.lock:
            if not self.connected():
                raise SpotifyError("управление Spotify не подключено: откройте страницу сервиса и войдите")
            if time.time() > self.tokens.get("expires_at", 0):
                self.save(self.token_request({"grant_type": "refresh_token", "refresh_token": self.tokens["refresh_token"]}))
            return self.tokens["access_token"]

    def request(self, method, path, query=None, body=None):
        if time.monotonic() < self.retry_after:
            raise SpotifyError("Spotify ограничил частоту запросов; повторите позже")
        url = f"{self.api}{path}" + (f"?{urllib.parse.urlencode(query)}" if query else "")
        try:
            return http_json(method, url, body=body, proxy=PROXY, timeout=4,
                             headers={"Authorization": f"Bearer {self.access_token()}"})
        except urllib.error.HTTPError as error:
            detail = error.read().decode()[:200]
            if error.code == 429:
                delay = error.headers.get("Retry-After", "30")
                self.retry_after = time.monotonic() + (int(delay) if delay.isdigit() else 30)
                raise SpotifyError("Spotify ограничил частоту запросов; повторите позже", 429) from None
            if error.code in (401, 403):
                raise SpotifyError("Spotify отказал в доступе: проверьте Premium, доступ аккаунта к приложению "
                                   "и разрешения; при необходимости подключите Spotify повторно", error.code) from None
            if error.code == 404:
                self.device = None
            raise SpotifyError(f"Spotify ответил {error.code}: {detail}", error.code) from None
        except OSError as error:
            raise SpotifyError(f"Spotify недоступен: {error}") from None

    def device_id(self):
        if self.device:
            return self.device
        devices = (self.request("GET", "/me/player/devices") or {}).get("devices", [])
        matches = [device for device in devices if device.get("name") == NAME and device.get("id")]
        if len(matches) > 1:
            raise SpotifyError(f"В Spotify несколько устройств «{NAME}». Задайте колонке уникальное имя, чтобы не управлять другим устройством.")
        if matches:
            self.device = matches[0]["id"]
            return self.device
        raise SpotifyError(f"колонки «{NAME}» нет среди устройств Spotify: librespot не запущен или не вошёл")

    def call(self, action, uri=None, kind=None):
        device = {"device_id": self.device_id()}
        if action == "play":
            body = {"uris": [uri]} if kind == "track" else {"context_uri": uri}
            self.request("PUT", "/me/player/play", device, body)
        elif action == "take_over":
            # a play command reaches a speaker that is not the active device with nothing to resume
            self.request("PUT", "/me/player", body={"device_ids": [device["device_id"]], "play": True})
        elif action == "resume":
            self.request("PUT", "/me/player/play", device)
        elif action == "pause":
            self.request("PUT", "/me/player/pause", device)
        elif action == "next":
            self.request("POST", "/me/player/next", device)
        elif action == "previous":
            self.request("POST", "/me/player/previous", device)

    def try_call(self, action):
        try:
            self.call(action)
        except SpotifyError as error:
            log.warning("Spotify %s: %s", action, error)

    def find(self, query, kind):
        """The best match: the user's own playlists first for a playlist"""
        if kind == "playlist":
            playlists = (self.request("GET", "/me/playlists", {"limit": 50}) or {}).get("items", [])
            for playlist in playlists:
                if playlist and query.casefold() in (playlist.get("name") or "").casefold():
                    return {"uri": playlist["uri"], "title": f"плейлист «{playlist['name']}»"}
        answer = self.request("GET", "/search", {"q": query, "type": kind, "limit": 1, "market": "from_token"}) or {}
        items = [item for item in (answer.get(f"{kind}s") or {}).get("items", []) if item]
        if not items:
            return None
        item = items[0]
        title = f"«{item['name']}»"
        if kind in ("track", "album") and item.get("artists"):
            title += " — " + ", ".join(artist["name"] for artist in item["artists"])
        return {"uri": item["uri"], "title": title}


def integer_ms(value):
    try:
        parsed = int(value)
        return parsed if 0 <= parsed <= 86400000 else None
    except (ValueError, TypeError):
        return None


def music_track(item):
    """Only documented catalog fields; no credentials or untrusted URL passthrough."""
    if not isinstance(item, dict):
        return None
    uri = item.get("uri", "")
    if not re.fullmatch(r"spotify:track:[A-Za-z0-9]{22}", uri):
        return None
    return {"uri": uri, "name": item.get("name", ""),
            "artists": ", ".join(artist.get("name", "") for artist in item.get("artists", [])),
            "album": (item.get("album") or {}).get("name", ""),
            "duration_ms": integer_ms(item.get("duration_ms")),
            "url": "https://open.spotify.com/track/" + uri.rsplit(":", 1)[1]}


class MusicService:
    """Dashboard reads/actions; no network calls from librespot's event callback.

    API requests, cache and lyrics use their own lock, never the PCM/playback lock.
    Spotify queue/state are account-wide, so verify the active device before use.
    """

    def __init__(self, owner):
        self.owner = owner
        self.lock = threading.RLock()
        self.cache = collections.OrderedDict()
        self.lyrics_retry_after = 0

    def cached(self, key, seconds, fetch):
        with self.lock:
            entry = self.cache.get(key)
            if entry and entry[0] > time.monotonic():
                if isinstance(entry[1], Exception):
                    raise entry[1]
                return entry[1]
            try:
                value = fetch()
            except SpotifyError as error:
                self.cache[key] = (time.monotonic() + 15, error)
                while len(self.cache) > 64:
                    self.cache.popitem(last=False)
                raise
            self.cache[key] = (time.monotonic() + seconds, value)
            self.cache.move_to_end(key)
            while len(self.cache) > 64:
                self.cache.popitem(last=False)
            return value

    def authorization(self):
        api = self.owner.api
        scopes = set((api.tokens or {}).get("scope", "").split()) if api else set()
        missing = sorted(set(SCOPES.split()) - scopes)
        return {"configured": bool(api), "connected": bool(api and api.connected()),
                "missing_scopes": missing, "reconnect_required": bool(missing),
                "authorize_url": api.authorize_url() if api else None,
                "redirect_uri": REDIRECT_URI if api else None}

    def require_api(self, scope=None):
        api = self.owner.api
        if not api or not api.connected():
            raise SpotifyError("Подключите управление Spotify в этом плеере. Нужен аккаунт Premium и доступ к приложению.")
        if scope and scope not in (api.tokens or {}).get("scope", "").split():
            raise SpotifyError("Для этой функции нужно повторно подключить Spotify и разрешить доступ к медиатеке.")
        return api

    def local(self):
        with self.owner.lock:
            track = dict(self.owner.track or {})
            active = self.owner.airplay != "playing" and self.owner.state != "stopped"
            playing = active and self.owner.state == "playing"
            position_clock = time.monotonic()
            if self.owner.listener and self.owner.listener.stalled:
                playing = False
                position_clock = self.owner.listener.stalled_at or track.get("position_at", position_clock)
            if not active or not track:
                return {"track": None, "position_ms": None, "duration_ms": None, "playing": False,
                        "source": "AirPlay" if self.owner.airplay == "playing" else "Spotify"}
            position = track.get("position_ms")
            duration = track.get("duration_ms")
            if position is not None and self.owner.state == "playing":
                position += max(0, int((position_clock - track.get("position_at", position_clock)) * 1000))
            if position is not None and duration is not None:
                position = min(position, duration)
            uri = track.get("uri", "")
            return {"track": {"uri": uri, "name": track.get("name", ""), "artists": track.get("artists", ""),
                              "album": track.get("album") or "", "duration_ms": duration,
                              "url": "https://open.spotify.com/track/" + uri.rsplit(":", 1)[-1]
                              if re.fullmatch(r"spotify:track:[A-Za-z0-9]{22}", uri) else None},
                    "position_ms": position, "duration_ms": duration, "playing": playing, "source": "Spotify"}

    def playback(self, fresh=False):
        api = self.require_api()
        fetch = lambda: api.request("GET", "/me/player") or {}
        value = fetch() if fresh else self.cached("playback", 5, fetch)
        device = value.get("device") or {}
        if not device.get("id") or device["id"] != api.device_id():
            raise SpotifyError("Spotify сейчас не воспроизводится на выбранной колонке. Выберите её в Spotify Connect или включите найденный трек.")
        return value

    def read(self, operation, query):
        if operation == "identity":
            return {"duid": STATION_ID}
        if operation == "status":
            result = self.local()
            result.update({"authorization": self.authorization(), "capabilities": {}, "shuffle": None, "repeat": None})
            if result["source"] == "AirPlay":
                result["message"] = "Воспроизведением AirPlay управляет устройство-отправитель."
                return result
            try:
                playback = self.playback()
                blocked = (playback.get("actions") or {}).get("disallows") or playback.get("actions") or {}
                restricted = (playback.get("device") or {}).get("is_restricted", False)
                can = not restricted
                result["capabilities"] = {"seek": can and bool(result["track"]) and not blocked.get("seeking", False),
                                          "shuffle": can and not blocked.get("toggling_shuffle", False),
                                          "repeat": can and not blocked.get("toggling_repeat_context", False),
                                          "queue": can}
                result["shuffle"] = playback.get("shuffle_state")
                result["repeat"] = playback.get("repeat_state")
                # Never resurrect a stopped stream from Spotify's account-wide
                # last-played item; title and timing originate in this bridge.
                if restricted:
                    result["message"] = "Spotify пометил это устройство как ограниченное: управление Web API недоступно."
            except SpotifyError as error:
                result["message"] = str(error)
            return result
        if operation == "search":
            api = self.require_api()
            term = query.get("q", "").strip()
            if not 1 <= len(term) <= 200:
                raise SpotifyError("Введите название трека или исполнителя (до 200 символов).")
            value = self.cached(("search", term), 60,
                                lambda: api.request("GET", "/search", {"q": term, "type": "track", "limit": 10})) or {}
            return {"tracks": [track for item in (value.get("tracks") or {}).get("items", [])
                               if (track := music_track(item))]}
        if operation == "lyrics":
            return self.lyrics()
        if operation == "queue":
            self.playback()
            value = self.cached("queue", 10, lambda: self.require_api().request("GET", "/me/player/queue")) or {}
            return {"tracks": [track for item in value.get("queue", [])[:50] if (track := music_track(item))],
                    "message": "Spotify отдаёт ближайшую часть очереди. Перестановка и удаление через Web API не поддерживаются."}
        if operation == "saved":
            uri = self.track_uri(query.get("uri"))
            api = self.require_api("user-library-read")
            answer = self.cached(("saved", uri), 30, lambda: api.request("GET", "/me/library/contains", {"uris": uri}))
            return {"saved": answer[0] if isinstance(answer, list) and answer else None}
        if operation in ("artist", "related"):
            playback = self.playback()
            item = playback.get("item") or {}
            api = self.require_api()
            if operation == "related":
                track_id = self.track_uri(item.get("uri")).split(":")[-1]
                try:
                    value = self.cached(("related", track_id), 300,
                                        lambda: api.request("GET", "/recommendations", {"seed_tracks": track_id, "limit": 10})) or {}
                    return {"tracks": [track for item in value.get("tracks", []) if (track := music_track(item))]}
                except SpotifyError as error:
                    if error.status in (403, 404):
                        raise SpotifyError("Spotify не предоставляет рекомендации этому приложению: endpoint закрыт для новых/Development Mode приложений. Поиск треков остаётся доступен.") from None
                    raise
            artists = item.get("artists") or []
            artist_id = artists[0].get("id", "") if artists else ""
            if not re.fullmatch(r"[A-Za-z0-9]{22}", artist_id):
                raise SpotifyError("Spotify не передал исполнителя для текущего трека.")
            value = self.cached(("artist", artist_id), 300, lambda: api.request("GET", f"/artists/{artist_id}")) or {}
            return {"name": value.get("name"), "genres": value.get("genres") or [],
                    "followers": (value.get("followers") or {}).get("total"),
                    "url": f"https://open.spotify.com/artist/{artist_id}",
                    "message": "Spotify не предоставляет биографию. Жанры и число слушателей могут отсутствовать в вашем режиме API."}
        raise SpotifyError("Неизвестный запрос музыки")

    @staticmethod
    def track_uri(value):
        if not isinstance(value, str) or not re.fullmatch(r"spotify:track:[A-Za-z0-9]{22}", value):
            raise SpotifyError("Нужен Spotify URI трека")
        return value

    def action(self, body):
        action = body.get("action")
        if action == "authorize":
            if not self.owner.api:
                raise SpotifyError("Spotify API не настроен: используются существующие SPOTIFY_CLIENT_ID и SPOTIFY_CLIENT_SECRET.")
            url = body.get("url")
            if not isinstance(url, str) or len(url) > 8192:
                raise SpotifyError("Вставьте адрес перенаправления после входа в Spotify.")
            self.owner.api.finish_authorization(url)
        else:
            api = self.require_api()
            if action == "save":
                self.require_api("user-library-modify")
                uri = self.track_uri(body.get("uri"))
                if not isinstance(body.get("saved"), bool):
                    raise SpotifyError("Укажите состояние любимого трека")
                api.request("PUT" if body["saved"] else "DELETE", "/me/library", {"uris": uri})
            elif action == "play":
                api.call("play", self.track_uri(body.get("uri")), "track")
            elif action in ("seek", "shuffle", "repeat", "queue_add"):
                playback = self.playback(fresh=True)
                if (playback.get("device") or {}).get("is_restricted"):
                    raise SpotifyError("Spotify запретил управление этим устройством")
                blocked = (playback.get("actions") or {}).get("disallows") or playback.get("actions") or {}
                restriction = {"seek": "seeking", "shuffle": "toggling_shuffle", "repeat":
                               "toggling_repeat_track" if body.get("mode") == "track" else "toggling_repeat_context"}.get(action)
                if restriction and blocked.get(restriction):
                    raise SpotifyError("Spotify временно запретил это действие для текущего трека")
                expected = body.get("expected_uri")
                if expected and expected != (playback.get("item") or {}).get("uri"):
                    raise SpotifyError("Трек сменился. Дождитесь обновления плеера и повторите действие.")
                query = {"device_id": api.device_id()}
                if action == "seek":
                    position = body.get("position_ms")
                    duration = (playback.get("item") or {}).get("duration_ms", 0)
                    if isinstance(position, bool) or not isinstance(position, int) or not 0 <= position <= duration:
                        raise SpotifyError("Позиция должна находиться внутри текущего трека")
                    query["position_ms"] = position
                    api.request("PUT", "/me/player/seek", query)
                elif action == "shuffle":
                    if not isinstance(body.get("enabled"), bool):
                        raise SpotifyError("Укажите состояние случайного порядка")
                    query["state"] = str(body["enabled"]).lower()
                    api.request("PUT", "/me/player/shuffle", query)
                elif action == "repeat":
                    if body.get("mode") not in ("off", "track", "context"):
                        raise SpotifyError("Неизвестный режим повтора")
                    query["state"] = body["mode"]
                    api.request("PUT", "/me/player/repeat", query)
                else:
                    query["uri"] = self.track_uri(body.get("uri"))
                    api.request("POST", "/me/player/queue", query)
            else:
                raise SpotifyError("Неизвестное действие музыки")
        with self.lock:
            self.cache.clear()
        return {"ok": True, "message": "Spotify принял запрос"}

    def lyrics(self):
        local = self.local()
        track = local["track"]
        if not track:
            raise SpotifyError("Выберите трек Spotify для поиска текста")
        key = ("lyrics", track["name"], track["artists"], track["album"], track["duration_ms"])
        def fetch():
            if time.monotonic() < self.lyrics_retry_after:
                raise SpotifyError("LRCLIB ограничил запросы. Повторите позже.")
            query = {"track_name": track["name"], "artist_name": track["artists"], "album_name": track["album"]}
            if track["duration_ms"] is not None:
                query["duration"] = round(track["duration_ms"] / 1000)
            url = "https://lrclib.net/api/get?" + urllib.parse.urlencode(query)
            try:
                value = http_json("GET", url, timeout=5, proxy=PROXY,
                                  headers={"User-Agent": "alice-server-rs/1.0 (lyrics dashboard)", "Accept": "application/json"}) or {}
            except urllib.error.HTTPError as error:
                if error.code == 404:
                    return {"source": "LRCLIB", "lines": [], "plain": None, "message": "Точный текст этого трека в LRCLIB не найден."}
                if error.code == 429:
                    delay = error.headers.get("Retry-After", "60")
                    self.lyrics_retry_after = time.monotonic() + (int(delay) if delay.isdigit() else 60)
                raise SpotifyError("LRCLIB временно недоступен. Попробуйте позже.") from None
            except (OSError, ValueError):
                raise SpotifyError("Не удалось получить текст из LRCLIB") from None
            lines = []
            offset = re.search(r"\[offset:([+-]?\d+)\]", value.get("syncedLyrics") or "")
            offset_ms = int(offset[1]) if offset else 0
            for line in (value.get("syncedLyrics") or "").splitlines()[:3000]:
                text = re.sub(r"\[[^]]+\]", "", line).strip()
                for minutes, seconds in re.findall(r"\[(\d{1,3}):(\d{2}(?:\.\d{1,3})?)\]", line):
                    lines.append({"at_ms": max(0, round((int(minutes) * 60 + float(seconds)) * 1000) - offset_ms), "text": text})
            lines.sort(key=lambda line: line["at_ms"])
            return {"source": "LRCLIB", "url": "https://lrclib.net", "lines": lines,
                    "plain": (value.get("plainLyrics") or "")[:100000] or None,
                    "message": "Инструментальная композиция" if value.get("instrumental") else None}
        return self.cached(key, 3600, fetch)


bridge = None


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def do_GET(self):
        request = urllib.parse.urlsplit(self.path)
        path = request.path
        if path == "/stream.mp3":
            self.send_response(200)
            self.send_header("Content-Type", "audio/mpeg")
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            bridge.serve_listener(self)
        elif path.startswith("/cover/"):
            if urllib.parse.parse_qs(request.query).get("duid") != [STATION_ID]:
                return self.reply(404, "text/plain", "not found")
            try:
                content_type, body = bridge.covers.get(path.removeprefix("/cover/"))
            except KeyError:
                return self.reply(404, "text/plain", "not found")
            except (OSError, ValueError):
                return self.reply(502, "text/plain", "cover unavailable")
            self.reply(200, content_type, body, cache_control="private, max-age=3600, immutable")
        elif path == "/functions":
            self.reply(200, "application/json", json.dumps(bridge.catalog()))
        elif path == "/music":
            query = urllib.parse.parse_qs(request.query)
            if query.get("duid") != [STATION_ID]:
                return self.reply(404, "application/json", json.dumps({"error": "Колонка не обслуживается этим мостом"}))
            self.music_reply(lambda: bridge.music.read(query.get("op", ["status"])[0],
                                                       {key: values[0] for key, values in query.items()}))
        elif path == "/health":
            self.reply(200, "text/plain", "ok")
        elif path == "/":
            self.reply(200, "text/html; charset=utf-8", status_page())
        else:
            self.reply(404, "text/plain", "not found")

    def do_PATCH(self):
        if urllib.parse.urlsplit(self.path).path != "/functions":
            return self.reply(404, "text/plain", "not found")
        call = self.json_body()
        name, arguments = call.get("name", ""), call.get("arguments") or {}
        log.info("function %s %s", name, json.dumps(arguments, ensure_ascii=False))
        result = bridge.call(name, arguments if isinstance(arguments, dict) else {})
        log.info("function %s: %s", name, result)
        self.reply(200, "text/plain; charset=utf-8", result)

    def do_POST(self):
        request = urllib.parse.urlsplit(self.path)
        path = request.path
        if path == "/state":
            self.reply(200, "application/json", json.dumps(bridge.state_entries(), ensure_ascii=False))
        elif path == "/music":
            if urllib.parse.parse_qs(request.query).get("duid") != [STATION_ID]:
                return self.reply(404, "application/json", json.dumps({"error": "Колонка не обслуживается этим мостом"}))
            if int(self.headers.get("Content-Length") or 0) > 16384:
                return self.reply(413, "application/json", json.dumps({"error": "Запрос слишком большой"}))
            self.music_reply(lambda: bridge.music.action(self.json_body()))
        elif path == "/event" and self.client_address[0] in ("127.0.0.1", "::1"):
            bridge.on_event(self.json_body())
            self.reply(200, "text/plain", "ok")
        elif path == "/sign-in":
            with bridge.lock:
                bridge.sign_in_answer = self.form_value("url")
                bridge.lock.notify_all()
            self.redirect()
        elif path == "/api-sign-in" and bridge.api:
            try:
                bridge.api.finish_authorization(self.form_value("url"))
                bridge.events.append(f"{time.strftime('%H:%M:%S')} управление Spotify подключено")
            except SpotifyError as error:
                bridge.events.append(f"{time.strftime('%H:%M:%S')} ошибка подключения управления: {error}")
            self.redirect()
        else:
            self.reply(404, "text/plain", "not found")

    def music_reply(self, callback):
        try:
            self.reply(200, "application/json; charset=utf-8", json.dumps(callback(), ensure_ascii=False))
        except SpotifyError as error:
            self.reply(409, "application/json; charset=utf-8", json.dumps({"error": str(error)}, ensure_ascii=False))

    def json_body(self):
        try:
            value = json.loads(self.rfile.read(int(self.headers.get("Content-Length") or 0)) or b"{}")
            return value if isinstance(value, dict) else {}
        except ValueError:
            return {}

    def form_value(self, name):
        body = self.rfile.read(int(self.headers.get("Content-Length") or 0)).decode()
        return urllib.parse.parse_qs(body).get(name, [""])[0]

    def redirect(self):
        self.send_response(303)
        self.send_header("Location", "/")
        self.end_headers()

    def reply(self, status, content_type, text, cache_control=None):
        body = text if isinstance(text, bytes) else text.encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        if cache_control:
            self.send_header("Cache-Control", cache_control)
            self.send_header("X-Content-Type-Options", "nosniff")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def status_page():
    with bridge.lock:
        status, sign_in_url = bridge.librespot_status, bridge.sign_in_url
        events = list(bridge.events)
        listener = bridge.listener
    escape = html.escape
    parts = [f"<h1>Spotify на колонке «{escape(NAME)}»</h1>", f"<p>librespot: <b>{escape(status)}</b></p>"]
    if sign_in_url:
        parts.append(f"""<h2>Вход в Spotify</h2><ol>
<li>Откройте <a href="{escape(sign_in_url)}" target="_blank">ссылку для входа</a> и войдите в аккаунт Spotify Premium.</li>
<li>Браузер перейдёт на адрес <code>http://127.0.0.1/login?code=…</code> и покажет ошибку — так и должно быть. Скопируйте этот адрес целиком.</li>
<li>Вставьте его сюда:</li></ol>
<form method="post" action="/sign-in"><input name="url" size="80" placeholder="http://127.0.0.1/login?code=..."> <button>Войти</button></form>""")
    parts.append(f"<p>Сейчас: {escape(bridge.now_playing())}. Станция {'слушает поток' if listener and not listener.stalled else 'не слушает поток'}.</p>")
    if not bridge.api:
        parts.append("<p>Голосовое управление выключено: задайте SPOTIFY_CLIENT_ID и SPOTIFY_CLIENT_SECRET приложения с developer.spotify.com.</p>")
    elif not bridge.api.connected() or bridge.music.authorization()["missing_scopes"]:
        parts.append(f"""<h2>Голосовое управление</h2><ol>
<li>В настройках приложения на developer.spotify.com должен быть Redirect URI <code>{escape(REDIRECT_URI)}</code>.</li>
<li>Откройте <a href="{escape(bridge.api.authorize_url())}" target="_blank">ссылку</a> и разрешите доступ.</li>
<li>Браузер перейдёт на <code>{escape(REDIRECT_URI)}?code=…</code> и покажет ошибку. Скопируйте этот адрес сюда:</li></ol>
<form method="post" action="/api-sign-in"><input name="url" size="80"> <button>Подключить</button></form>""")
    else:
        parts.append("<p>Голосовое управление подключено.</p>")
    if events:
        parts.append("<h2>События</h2><ul>" + "".join(f"<li>{escape(event)}</li>" for event in reversed(events)) + "</ul>")
    return ("<!doctype html><html lang=ru><meta charset=utf-8><meta name=viewport content='width=device-width, initial-scale=1'>"
            "<title>Spotify на колонке</title><style>body{font:16px/1.5 system-ui,sans-serif;max-width:760px;margin:24px auto;padding:0 16px}"
            "input{max-width:100%}code{word-break:break-all}</style>" + "".join(parts) + "</html>")


class Server(http.server.ThreadingHTTPServer):
    daemon_threads = True


def main():
    global bridge
    if not STREAM_URL:
        sys.exit("STREAM_URL is required: the address of this service's /stream.mp3 as the station reaches it")
    if not ALICE_API_URL:
        log.warning("ALICE_API_URL is not set: the station will not follow Spotify")
    bridge = Bridge()
    targets = [bridge.run_librespot, bridge.run_encoder, bridge.run_pump]
    if AIRPLAY_FIFO:
        targets += [bridge.run_airplay, bridge.run_airplay_volume]
    for target in targets:
        threading.Thread(target=target, daemon=True).start()
    log.info("Spotify bridge for «%s» on port %d, stream %s, station %s", NAME, PORT, STREAM_URL, STATION_ID)
    # as PID 1 of a container the process ignores SIGTERM without a handler
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    Server(("0.0.0.0", PORT), Handler).serve_forever()


if __name__ == "__main__":
    main()
