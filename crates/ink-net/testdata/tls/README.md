# TEST-ONLY TLS material

`test-ca.pem` is a throwaway certificate authority and `test-server.pem` /
`test-server.key.pem` a server certificate it signed, for `ink_net::testserver` (host
unit tests and the emulator selftest). SANs: `127.0.0.1`, `10.0.2.2` (host seen from the
emulator over Wi-Fi), `192.168.15.201` (host over USB networking), `localhost`.

The CA's private key was deleted after signing, so nothing else can be signed with it.
No rinky build trusts this CA unless a test passes it explicitly
(`UreqClient::with_ca_pem`). The key protects nothing; do not reuse it anywhere.

Regenerate with `./gen.sh` (OpenSSL 3.4+). Valid 2026-01-01 to 2125-12-31.
