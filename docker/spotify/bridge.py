#!/usr/bin/env python3
"""Spotify on the station through alice-server.

librespot (https://github.com/librespot-org/librespot) runs as a Spotify
Connect speaker. Its PCM is encoded to MP3 by ffmpeg and served at
/stream.mp3, which the station plays with audio_play: when Spotify starts
playing on the speaker the station is told to play the stream, when Spotify
pauses or moves to another device the station pauses (push-directive of
alice-server's management API). While the station does not read the stream
(its own pause), librespot is held back until it reads again.

The first start needs a Spotify sign-in: the page at / shows the link and
takes the address Spotify redirects to afterwards. With SPOTIFY_CLIENT_ID and
SPOTIFY_CLIENT_SECRET of an app on developer.spotify.com the assistant also
gets voice control: search and play, pause, next and previous track (the
function server protocol of alice-server at /functions, the playback state at
/state).

librespot runs this file as its --onevent program too: with PLAYER_EVENT in
the environment it only hands the event over to the running bridge.
"""
import base64
import collections
import html
import http.server
import json
import logging
import os
import secrets
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

PORT = int(os.environ.get("PORT") or 8090)
# what librespot reports through --onevent
EVENT_FIELDS = ("PLAYER_EVENT", "TRACK_ID", "URI", "NAME", "ARTISTS", "ALBUM", "DURATION_MS", "POSITION_MS", "VOLUME", "CLIENT_NAME")


def hand_over_event():
    """The --onevent mode: the event goes to the bridge, whatever happens to it"""
    fields = {name: os.environ[name] for name in EVENT_FIELDS if name in os.environ}
    request = urllib.request.Request(f"http://127.0.0.1:{PORT}/event", data=json.dumps(fields).encode(),
                                     headers={"Content-Type": "application/json"}, method="POST")
    try:
        urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=5).read()
    except OSError:
        pass


if __name__ == "__main__" and "PLAYER_EVENT" in os.environ:
    hand_over_event()
    sys.exit(0)


def env(name, default=None):
    return (os.environ.get(name) or "").strip() or default


NAME = env("SPOTIFY_NAME", "Алиса")
STREAM_URL = env("STREAM_URL")
ALICE_API_URL = (env("ALICE_API_URL") or "").rstrip("/")
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

RATE = 44100 * 2 * 2  # bytes per second of 16-bit stereo PCM at 44.1 kHz
CHUNK = RATE // 20  # 50 ms
# how far the stream may run ahead of real time, so the station starts with a buffer
LEAD = float(env("LEAD_MS", "2000")) / 1000
# a station that takes no data for this long has paused its player
STALL = float(env("STALL_MS", "2000")) / 1000
SCOPES = "user-read-playback-state user-modify-playback-state user-read-currently-playing playlist-read-private"

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
        text = response.read().decode()
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


class Station:
    """The station, through the management API of alice-server"""

    def push(self, name, payload, led_silent):
        if not ALICE_API_URL:
            log.warning("ALICE_API_URL is not set: cannot tell the station to %s", name)
            return
        directive = {"Name": name, "Type": "client_action", "Payload": struct(payload), "IsLedSilent": led_silent}
        try:
            http_json("POST", f"{ALICE_API_URL}/devices/{urllib.parse.quote(STATION_ID)}/push-directive", body=directive)
            log.info("station %s: %s", STATION_ID, name)
        except (OSError, ValueError) as error:
            log.warning("pushing %s to the station failed: %s", name, error)

    def play(self):
        url = f"{STREAM_URL}{'&' if '?' in STREAM_URL else '?'}session={uuid.uuid4().hex}"
        stream = {"format": "MP3", "id": str(uuid.uuid4()), "type": "Track", "url": url}
        self.push("audio_play", {"background_mode": "Ducking", "callbacks": {}, "screen_type": "Music", "stream": stream}, False)

    def pause(self):
        self.push("player_pause", {}, True)


class Listener:
    """The station's connection to /stream.mp3"""

    def __init__(self, address):
        self.address = address
        self.chunks = collections.deque()
        self.closed = False
        self.stalled = False


class Bridge:
    def __init__(self):
        self.lock = threading.Condition()
        self.station = Station()
        self.listener = None
        self.state = "stopped"
        self.track = None
        self.last_play_push = None
        self.librespot_status = "starting"
        self.sign_in_url = None
        self.sign_in_answer = None
        self.events = collections.deque(maxlen=20)
        self.api = SpotifyApi() if CLIENT_ID and CLIENT_SECRET else None
        self.encoder = None

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
        command = self.librespot_command("--backend", "pipe", "--device", str(FIFO), "--format", "S16", "--bitrate", "320",
                                         "--initial-volume", "100", "--onevent", str(Path(__file__).resolve()))
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
        code = process.wait()
        with self.lock:
            self.librespot_status = f"перезапуск (код {code})"
        log.warning("librespot exited with %s", code)

    # sound

    def run_encoder(self):
        while True:
            command = ["ffmpeg", "-loglevel", "error", "-f", "s16le", "-ar", "44100", "-ac", "2", "-i", "pipe:0",
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
                started, sent = time.monotonic(), 0
                while True:
                    waited = self.wait_for_listener()
                    started += waited
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
        """Seconds spent waiting for a station that reads the stream"""
        started = time.monotonic()
        with self.lock:
            while not (self.listener and not self.listener.closed and not self.listener.stalled):
                self.lock.wait(1)
        return time.monotonic() - started

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
        paused: librespot is held back, and with the Web API Spotify pauses"""
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
            playing = self.state == "playing"
        log.info("the station paused the stream")
        if playing and self.api:
            self.api.try_call("pause")

    def station_resumed(self, listener):
        with self.lock:
            listener.stalled = False
            self.lock.notify_all()
            paused = self.state != "playing"
        log.info("the station reads the stream again")
        if paused and self.api:
            self.api.try_call("resume")

    def listening(self):
        """Whether the station reads the stream now"""
        with self.lock:
            return bool(self.listener and not self.listener.closed and not self.listener.stalled)

    # librespot's events

    def on_event(self, fields):
        event = fields.get("PLAYER_EVENT", "")
        if event in ("position_correction", "preloading", "preload_next", "play_request_id_changed", "sink"):
            return
        with self.lock:
            self.events.append(f"{time.strftime('%H:%M:%S')} {event}")
            if event == "track_changed":
                artists = ", ".join(filter(None, fields.get("ARTISTS", "").split("\n")))
                self.track = {"name": fields.get("NAME", ""), "artists": artists, "uri": fields.get("URI", "")}
            elif event == "playing":
                self.state = "playing"
            elif event == "paused":
                self.state = "paused"
            elif event in ("stopped", "session_disconnected"):
                self.state = "stopped"
            state = self.state
        log.info("librespot event %s", event)
        if event == "playing" and not self.listening():
            # the station plays the stream; a push that is still connecting is not repeated
            with self.lock:
                recent = (self.last_play_push is not None and time.monotonic() - self.last_play_push < 5
                          and self.listener is None)
                if not recent:
                    self.last_play_push = time.monotonic()
            if not recent:
                self.station.play()
        elif event in ("paused", "stopped", "session_disconnected") and state != "playing":
            self.station.pause()

    # the assistant's functions and state (alice-server's protocols)

    def catalog(self):
        functions = {
            "spotify_play": {
                "description": "Play music on this speaker from Spotify (has response). Without a query it resumes what was "
                               "playing; with one it searches Spotify. Use it for any request to play or resume music.",
                "arguments": {
                    "query": {"type": "string", "description": "What to play: a track, an artist, an album or a playlist, as the user named it"},
                    "type": {"type": "string", "enum": ["track", "artist", "album", "playlist"], "description": "What the query names; track if unclear"},
                },
                "hasResponse": True,
            },
            "spotify_pause": {"description": "Pause the Spotify music on this speaker; use it rather than smart_speaker_player_pause "
                                             "while Spotify plays", "arguments": {}},
            "spotify_now_playing": {"description": "What plays in Spotify on this speaker now (has response)", "arguments": {}, "hasResponse": True},
        }
        if self.api:
            functions["spotify_next"] = {"description": "Next track in Spotify on this speaker", "arguments": {}}
            functions["spotify_previous"] = {"description": "Previous track in Spotify on this speaker", "arguments": {}}
        return functions

    def call(self, name, arguments):
        try:
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
            if name == "spotify_now_playing":
                return self.now_playing()
            return f"error: no function {name}"
        except SpotifyError as error:
            return f"error: {error}"

    def play(self, query, kind):
        if not query:
            if self.api:
                self.api.call("resume")
            else:
                self.station.play()
            return "ok: продолжаю"
        if not self.api:
            return "error: голосовой поиск в Spotify не подключён (нужны SPOTIFY_CLIENT_ID и SPOTIFY_CLIENT_SECRET)"
        found = self.api.find(query, kind)
        if not found:
            return f"error: в Spotify ничего не нашлось по запросу «{query}»"
        self.api.call("play", found["uri"], kind)
        return f"ok: включаю {found['title']}"

    def now_playing(self):
        with self.lock:
            track, state = self.track, self.state
        if not track or state == "stopped":
            return "В Spotify на колонке сейчас ничего не играет"
        title = f"«{track['name']}» — {track['artists']}" if track["artists"] else f"«{track['name']}»"
        return f"Сейчас играет {title}" if state == "playing" else f"На паузе {title}"

    def state_entries(self):
        return {"spotify": {"description": "Spotify music on this speaker", "value": self.now_playing()}}


class SpotifyError(Exception):
    pass


class SpotifyApi:
    """The Web API of the user's own app on developer.spotify.com"""

    accounts = "https://accounts.spotify.com"
    api = "https://api.spotify.com/v1"

    def __init__(self):
        self.lock = threading.Lock()
        self.tokens = json.loads(TOKENS.read_text()) if TOKENS.is_file() else None
        self.state = secrets.token_urlsafe(16)
        self.device = None

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
        if not self.connected():
            raise SpotifyError("управление Spotify не подключено: откройте страницу сервиса и войдите")
        if time.time() > self.tokens.get("expires_at", 0):
            self.save(self.token_request({"grant_type": "refresh_token", "refresh_token": self.tokens["refresh_token"]}))
        return self.tokens["access_token"]

    def request(self, method, path, query=None, body=None):
        url = f"{self.api}{path}" + (f"?{urllib.parse.urlencode(query)}" if query else "")
        try:
            return http_json(method, url, body=body, proxy=PROXY, headers={"Authorization": f"Bearer {self.access_token()}"})
        except urllib.error.HTTPError as error:
            detail = error.read().decode()[:200]
            if error.code == 404:
                self.device = None
            raise SpotifyError(f"Spotify ответил {error.code}: {detail}") from None
        except OSError as error:
            raise SpotifyError(f"Spotify недоступен: {error}") from None

    def device_id(self):
        if self.device:
            return self.device
        devices = (self.request("GET", "/me/player/devices") or {}).get("devices", [])
        for device in devices:
            if device.get("name") == NAME:
                self.device = device["id"]
                return self.device
        raise SpotifyError(f"колонки «{NAME}» нет среди устройств Spotify: librespot не запущен или не вошёл")

    def call(self, action, uri=None, kind=None):
        device = {"device_id": self.device_id()}
        if action == "play":
            body = {"uris": [uri]} if kind == "track" else {"context_uri": uri}
            self.request("PUT", "/me/player/play", device, body)
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


bridge = None


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def do_GET(self):
        path = urllib.parse.urlsplit(self.path).path
        if path == "/stream.mp3":
            self.send_response(200)
            self.send_header("Content-Type", "audio/mpeg")
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            bridge.serve_listener(self)
        elif path == "/functions":
            self.reply(200, "application/json", json.dumps(bridge.catalog()))
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
        path = urllib.parse.urlsplit(self.path).path
        if path == "/state":
            self.reply(200, "application/json", json.dumps(bridge.state_entries(), ensure_ascii=False))
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

    def reply(self, status, content_type, text):
        body = text.encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
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
    elif not bridge.api.connected():
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
    for target in (bridge.run_librespot, bridge.run_encoder, bridge.run_pump):
        threading.Thread(target=target, daemon=True).start()
    log.info("Spotify bridge for «%s» on port %d, stream %s, station %s", NAME, PORT, STREAM_URL, STATION_ID)
    # as PID 1 of a container the process ignores SIGTERM without a handler
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    Server(("0.0.0.0", PORT), Handler).serve_forever()


if __name__ == "__main__":
    main()
