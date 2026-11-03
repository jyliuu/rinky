#!/usr/bin/env python3
"""Mock GitHub release server for the in-app update tests. Binds 127.0.0.1 only.

  mock-release-server.py --package rinky-V-kindlehf.tar.gz --tag vV
                         [--port 0] [--public-host 127.0.0.1] [--port-file FILE]
                         [--bad-checksum] [--no-checksum] [--digest] [--slow SECONDS]

Routes (the shapes GitHub uses):
  GET /repos/rinky/rinky/releases/latest   release JSON: tag_name, assets[]
  GET /download/<tag>/<package>                    302 to /objects/<package>
  GET /objects/<package>                           the package bytes
  GET /download/<tag>/SHA256SUMS                   "<sha256>  <package>"

--bad-checksum  SHA256SUMS names a wrong SHA-256 (the update must refuse)
--no-checksum   no SHA256SUMS asset and no digest (the update must refuse)
--digest        also put "digest": "sha256:<hex>" on the package asset
--public-host   host name the client uses in asset URLs (10.0.2.2 from the emulator)
--slow S        send the package bytes spread over about S seconds (two updates at once
                then surely overlap)

Prints "port N" when listening and logs one line per request to stderr.
"""

from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
import sys
import time


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--package", required=True)
    ap.add_argument("--tag", required=True)
    ap.add_argument("--port", type=int, default=0)
    ap.add_argument("--public-host", default="127.0.0.1")
    ap.add_argument("--port-file")
    ap.add_argument("--bad-checksum", action="store_true")
    ap.add_argument("--no-checksum", action="store_true")
    ap.add_argument("--digest", action="store_true")
    ap.add_argument("--slow", type=float, default=0.0)
    a = ap.parse_args()

    name = os.path.basename(a.package)
    with open(a.package, "rb") as f:
        data = f.read()
    sha = hashlib.sha256(data).hexdigest()
    sums_hash = hashlib.sha256(b"not the package").hexdigest() if a.bad_checksum else sha
    sums = f"{sums_hash}  {name}\n".encode()

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, fmt: str, *args: object) -> None:
            sys.stderr.write("mock-release: %s %s\n" % (self.address_string(), fmt % args))

        def send(self, status: int, body: bytes, ctype: str = "application/octet-stream", extra: dict | None = None) -> None:
            self.send_response(status)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            for k, v in (extra or {}).items():
                self.send_header(k, v)
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self) -> None:  # noqa: N802 (http.server API)
            base = f"http://{a.public_host}:{self.server.server_address[1]}"
            if self.path == "/repos/rinky/rinky/releases/latest":
                asset = {"name": name, "size": len(data), "browser_download_url": f"{base}/download/{a.tag}/{name}"}
                if a.digest and not a.no_checksum:
                    asset["digest"] = f"sha256:{sums_hash}"
                assets = [asset]
                if not a.no_checksum:
                    assets.append({"name": "SHA256SUMS", "size": len(sums), "browser_download_url": f"{base}/download/{a.tag}/SHA256SUMS"})
                body = json.dumps({"tag_name": a.tag, "draft": False, "prerelease": False, "assets": assets}).encode()
                self.send(200, body, "application/json")
            elif self.path == f"/download/{a.tag}/{name}":
                self.send(302, b"", extra={"Location": f"{base}/objects/{name}"})
            elif self.path == f"/objects/{name}" and a.slow > 0:
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(len(data)))
                self.send_header("Connection", "close")
                self.end_headers()
                steps = 20
                chunk = -(-len(data) // steps)
                for i in range(0, len(data), chunk):
                    self.wfile.write(data[i : i + chunk])
                    self.wfile.flush()
                    time.sleep(a.slow / steps)
            elif self.path == f"/objects/{name}":
                self.send(200, data)
            elif self.path == f"/download/{a.tag}/SHA256SUMS" and not a.no_checksum:
                self.send(200, sums, "text/plain")
            else:
                self.send(404, b"not found\n", "text/plain")

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", a.port), Handler)
    port = srv.server_address[1]
    if a.port_file:
        with open(a.port_file, "w") as f:
            f.write(f"{port}\n")
    print(f"port {port}", flush=True)
    sys.stderr.write(f"mock-release: {name} sha256 {sha} served as {a.tag}; SHA256SUMS says {sums_hash}\n")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
