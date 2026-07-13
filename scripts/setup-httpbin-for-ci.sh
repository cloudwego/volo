#!/bin/bash

set -o errexit
set -o nounset
set -o pipefail

HTTPBIN_IMAGE="${HTTPBIN_IMAGE:-ghcr.io/mccutchen/go-httpbin:2.20.0}"
HTTPBIN_HTTP_URL="${VOLO_HTTPBIN_BASE_URL:-http://127.0.0.1:8080}"
HTTPBIN_HTTPS_PORT="${HTTPBIN_HTTPS_PORT:-8443}"
HTTPBIN_HTTPS_URL="${VOLO_HTTPBIN_HTTPS_BASE_URL:-https://127.0.0.1:${HTTPBIN_HTTPS_PORT}}"
HTTPBIN_CA_CERT="${VOLO_HTTPBIN_CA_CERT:-/tmp/volo-httpbin.crt}"
HTTPBIN_KEY="${HTTPBIN_KEY:-/tmp/volo-httpbin.key}"
HTTPBIN_HTTPS_CONTAINER="${HTTPBIN_HTTPS_CONTAINER:-volo-httpbin-https}"

sudo rm -f "${HTTPBIN_CA_CERT}" "${HTTPBIN_KEY}"
openssl req -x509 -newkey rsa:2048 -nodes \
	-keyout "${HTTPBIN_KEY}" \
	-out "${HTTPBIN_CA_CERT}" \
	-days 1 \
	-subj "/CN=127.0.0.1" \
	-addext "subjectAltName=IP:127.0.0.1,DNS:localhost" \
	-addext "basicConstraints=critical,CA:FALSE" \
	-addext "keyUsage=critical,digitalSignature,keyEncipherment" \
	-addext "extendedKeyUsage=serverAuth"
chmod 644 "${HTTPBIN_CA_CERT}"
chmod 640 "${HTTPBIN_KEY}"
sudo chown root:65532 "${HTTPBIN_CA_CERT}" "${HTTPBIN_KEY}"

docker rm -f "${HTTPBIN_HTTPS_CONTAINER}" >/dev/null 2>&1 || true
docker run -d --rm \
	--name "${HTTPBIN_HTTPS_CONTAINER}" \
	-p "${HTTPBIN_HTTPS_PORT}:8443" \
	-v "${HTTPBIN_CA_CERT}:/tmp/volo-httpbin.crt:ro" \
	-v "${HTTPBIN_KEY}:/tmp/volo-httpbin.key:ro" \
	"${HTTPBIN_IMAGE}" \
	/bin/go-httpbin \
		-port=8443 \
		-https-cert-file=/tmp/volo-httpbin.crt \
		-https-key-file=/tmp/volo-httpbin.key

curl --noproxy 127.0.0.1,localhost --retry 10 --retry-all-errors --retry-delay 1 -fsS \
	"${HTTPBIN_HTTP_URL}/get" >/dev/null
curl --noproxy 127.0.0.1,localhost --retry 10 --retry-all-errors --retry-delay 1 -fskS \
	"${HTTPBIN_HTTPS_URL}/get" >/dev/null
