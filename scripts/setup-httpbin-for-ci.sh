#!/bin/bash

set -o errexit
set -o nounset
set -o pipefail

HTTPBIN_IMAGE="${HTTPBIN_IMAGE:-ghcr.io/mccutchen/go-httpbin:2.20.0}"
VOLO_HTTPBIN_BASE_URL="${VOLO_HTTPBIN_BASE_URL:-http://127.0.0.1:8080}"
VOLO_HTTPBIN_HTTPS_BASE_URL="${VOLO_HTTPBIN_HTTPS_BASE_URL:-https://127.0.0.1:8443}"
VOLO_HTTPBIN_CA_CERT="${VOLO_HTTPBIN_CA_CERT:-/tmp/volo-httpbin.crt}"
HTTPBIN_CA_CERT="${VOLO_HTTPBIN_CA_CERT}"
HTTPBIN_KEY="${HTTPBIN_KEY:-/tmp/volo-httpbin.key}"
export VOLO_HTTPBIN_BASE_URL VOLO_HTTPBIN_HTTPS_BASE_URL VOLO_HTTPBIN_CA_CERT
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY

# HTTP httpbin on 8080
docker rm -f volo-httpbin >/dev/null 2>&1 || true
docker run -d --rm --name volo-httpbin -p 8080:8080 "${HTTPBIN_IMAGE}"

# Self-signed server certificate for the HTTPS httpbin. It must be a leaf
# certificate (CA:FALSE + serverAuth), otherwise rustls rejects it.
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

# HTTPS httpbin on 8443
docker rm -f volo-httpbin-https >/dev/null 2>&1 || true
docker run -d --rm \
	--name volo-httpbin-https \
	-p 8443:8443 \
	-v "${HTTPBIN_CA_CERT}:/tmp/volo-httpbin.crt:ro" \
	-v "${HTTPBIN_KEY}:/tmp/volo-httpbin.key:ro" \
	"${HTTPBIN_IMAGE}" \
	/bin/go-httpbin \
		-port=8443 \
		-https-cert-file=/tmp/volo-httpbin.crt \
		-https-key-file=/tmp/volo-httpbin.key

curl --noproxy 127.0.0.1,localhost --retry 10 --retry-all-errors --retry-delay 1 -fsS \
	"${VOLO_HTTPBIN_BASE_URL}/get" >/dev/null
curl --noproxy 127.0.0.1,localhost --retry 10 --retry-all-errors --retry-delay 1 -fskS \
	"${VOLO_HTTPBIN_HTTPS_BASE_URL}/get" >/dev/null
