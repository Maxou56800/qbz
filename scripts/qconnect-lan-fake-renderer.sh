#!/usr/bin/env bash
# A fake Qobuz Connect LAN receiver, for exercising QBZ's controller half
# without a BluOS player: announces `_qobuz-connect._tcp` through Avahi and
# serves the three official endpoints. It never joins the cloud (it has no
# credentials of its own), so from QBZ's point of view it is exactly what a
# Bluesound Node is before an official app pairs it: on the network, not in
# the session. The handoff body it receives is logged with both JWTs redacted.
#
#   bash scripts/qconnect-lan-fake-renderer.sh [port] [friendly name]
#
# Needs avahi-publish-service (avahi-utils) and python3. Ctrl-C stops both.
set -euo pipefail
PORT="${1:-8765}"
NAME="${2:-Fake Node}"
UUID="${FAKE_DEVICE_UUID:-$(python3 -c 'import uuid; print(uuid.uuid4())')}"
command -v avahi-publish-service >/dev/null || { echo "avahi-publish-service not found (install avahi-utils)"; exit 1; }

avahi-publish-service "$NAME" _qobuz-connect._tcp "$PORT" "device_uuid=$UUID" "sdk_version=1.0.0" "path=" &
AVAHI_PID=$!
trap 'kill $AVAHI_PID 2>/dev/null || true' EXIT
echo "[fake-renderer] announcing '$NAME' device_uuid=$UUID on port $PORT"

NAME="$NAME" UUID="$UUID" PORT="$PORT" python3 - <<'PY'
import json, os, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

NAME, UUID, PORT = os.environ["NAME"], os.environ["UUID"], int(os.environ["PORT"])

class Handler(BaseHTTPRequestHandler):
    def _json(self, payload):
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path.rstrip("/").endswith("get-display-info"):
            return self._json({
                "friendly_name": NAME, "serial_number": UUID[:8],
                "brand_display_name": "QBZ", "model_display_name": "Fake receiver",
                "max_audio_quality": "UP_TO_HIRES_192", "type": "Streamer",
                "software_version": "0.0.1",
            })
        if self.path.rstrip("/").endswith("get-connect-info"):
            return self._json({"app_id": "fake-renderer-app-id", "current_session_id": None})
        self.send_error(404)

    def do_POST(self):
        if not self.path.rstrip("/").endswith("connect-to-qconnect"):
            return self.send_error(404)
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length)
        try:
            body = json.loads(raw)
            for key in ("jwt_api", "jwt_qconnect"):
                if isinstance(body.get(key), dict) and "jwt" in body[key]:
                    body[key]["jwt"] = f"<{len(body[key]['jwt'])} bytes redacted>"
            print("[fake-renderer] connect-to-qconnect:", json.dumps(body, indent=2), flush=True)
        except Exception as exc:  # noqa: BLE001
            print("[fake-renderer] connect-to-qconnect: unparsable body:", exc, flush=True)
        self._json({})

    def log_message(self, fmt, *args):
        print("[fake-renderer]", self.address_string(), fmt % args, flush=True)

print(f"[fake-renderer] serving on 0.0.0.0:{PORT}", flush=True)
HTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
PY
