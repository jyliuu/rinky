#!/bin/sh
# Regenerates the TEST-ONLY certificate authority and server certificate used by
# ink_net::testserver (host tests and the emulator selftest). Nothing outside the tests
# trusts this CA; the keys protect nothing. Needs OpenSSL 3.4+ (-not_before/-not_after).
set -eu
cd "$(dirname "$0")"
OPENSSL=${OPENSSL:-openssl}
"$OPENSSL" ecparam -name prime256v1 -genkey -noout | "$OPENSSL" pkcs8 -topk8 -nocrypt -out test-ca.key.pem
"$OPENSSL" req -x509 -new -key test-ca.key.pem -sha256 -subj "/CN=rinky TEST-ONLY CA" \
  -not_before 20260101000000Z -not_after 21251231000000Z \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -out test-ca.pem
"$OPENSSL" ecparam -name prime256v1 -genkey -noout | "$OPENSSL" pkcs8 -topk8 -nocrypt -out test-server.key.pem
"$OPENSSL" req -new -key test-server.key.pem -subj "/CN=rinky test server" -out server.csr
cat > server.ext <<EXT
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature
extendedKeyUsage=serverAuth
subjectAltName=IP:127.0.0.1,IP:10.0.2.2,IP:192.168.15.201,DNS:localhost
EXT
"$OPENSSL" x509 -req -in server.csr -CA test-ca.pem -CAkey test-ca.key.pem -CAcreateserial -sha256 \
  -not_before 20260101000000Z -not_after 21251231000000Z -extfile server.ext -out test-server.pem
rm -f server.csr server.ext test-ca.srl test-ca.key.pem
"$OPENSSL" x509 -in test-server.pem -noout -subject -issuer -dates -ext subjectAltName
