"""Mock HTTP server for tests/e2e/plugin-net.sh. Binds 127.0.0.1 on a free port (the
emulator guest reaches it as 10.0.2.2 over the emulated Wi-Fi), writes the port to the
file given as argv[1], and serves:
  GET  /sse     text/event-stream, three `delta` events 0.3 s apart, then `done`
  GET  /chunks  three chunks 0.3 s apart (chunked transfer encoding)
  POST /json    {"answer": 42, "echo": <question from the JSON body>}
"""
import http.server
import json
import sys
import time


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("mock: %s %s\n" % (self.address_string(), fmt % args))
        sys.stderr.flush()

    def do_GET(self):
        if self.path == "/sse":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            for word in ["one", "two", "three"]:
                self.wfile.write(("event: delta\ndata: %s\n\n" % word).encode())
                self.wfile.flush()
                time.sleep(0.3)
            self.wfile.write(b"event: done\ndata: [DONE]\n\n")
            self.wfile.flush()
            self.close_connection = True
        elif self.path == "/chunks":
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            for part in ["alpha", "beta", "gamma"]:
                data = part.encode()
                self.wfile.write(b"%x\r\n%s\r\n" % (len(data), data))
                self.wfile.flush()
                time.sleep(0.3)
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
        else:
            self.send_error(404)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        out = json.dumps({"answer": 42, "echo": body.get("question")}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(sys.argv[1], "w") as f:
    f.write(str(server.server_address[1]))
server.serve_forever()
