#!/bin/bash
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
pitr_tmp=$(mktemp -d)
mkdir "$pitr_tmp/certs" "$pitr_tmp/certs/CAs"
pitr_name="pitr-chaos-${pitr_tmp##*.}"
cleanup() {
    if docker inspect "$pitr_name" >/dev/null 2>&1; then
        docker rm -f "$pitr_name" >/dev/null
    fi
    rm -rf "$pitr_tmp"
}
trap cleanup EXIT
# Separate disposable CA + leaf cert so rustls/webpki and Python ssl both verify cleanly.
openssl req -new -newkey rsa:2048 -x509 -nodes -days 1 -subj /CN=hirsel-chaos-ca \
    -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign \
    -keyout "$pitr_tmp/ca.key" -out "$pitr_tmp/ca.crt" 2>"$pitr_tmp/openssl.log"
openssl req -new -newkey rsa:2048 -nodes -subj /CN=localhost \
    -keyout "$pitr_tmp/certs/private.key" -out "$pitr_tmp/leaf.csr" 2>>"$pitr_tmp/openssl.log"
printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n' >"$pitr_tmp/leaf.ext"
openssl x509 -req -in "$pitr_tmp/leaf.csr" -CA "$pitr_tmp/ca.crt" -CAkey "$pitr_tmp/ca.key" \
    -CAcreateserial -days 1 -extfile "$pitr_tmp/leaf.ext" \
    -out "$pitr_tmp/certs/public.crt" 2>>"$pitr_tmp/openssl.log"
cp "$pitr_tmp/ca.crt" "$pitr_tmp/certs/CAs/ca.crt"
# The unprivileged MinIO container must be able to read its disposable key.
chmod 644 "$pitr_tmp/certs/private.key" "$pitr_tmp/certs/public.crt" "$pitr_tmp/ca.crt" "$pitr_tmp/certs/CAs/ca.crt"
export MINIO_ROOT_USER="pitr-probe"
export MINIO_ROOT_PASSWORD="$(openssl rand -hex 24)"
export AWS_ACCESS_KEY_ID="$MINIO_ROOT_USER" AWS_SECRET_ACCESS_KEY="$MINIO_ROOT_PASSWORD"
pitr_image='bitnamilegacy/minio@sha256:a70e90bb70f4732221901de53502334cef58665c6c4275a014d47adad10e1883'
pitr_entrypoint=/opt/bitnami/minio/bin/minio
docker run --rm --entrypoint "$pitr_entrypoint" "$pitr_image" --version >&2
docker run --detach --name "$pitr_name" -e MINIO_ROOT_USER -e MINIO_ROOT_PASSWORD \
    --entrypoint "$pitr_entrypoint" -p 127.0.0.1::9000 --tmpfs /pitr-data:mode=1777,size=128m \
    -v "$pitr_tmp/certs:/pitr-certs:ro" "$pitr_image" \
    server /pitr-data --certs-dir /pitr-certs --address :9000 >/dev/null
endpoint="https://$(docker port "$pitr_name" 9000/tcp)"
for ((attempt=0; attempt<20; attempt++)); do
    if curl -fsS --cacert "$pitr_tmp/ca.crt" "$endpoint/minio/health/live" 2>/dev/null; then
        python3 "$script_dir/s3.py" --port "${endpoint##*:}" --ca "$pitr_tmp/ca.crt" \
            --cert "$pitr_tmp/certs/public.crt" --key "$pitr_tmp/certs/private.key" "$@"
        exit
    fi
    sleep 1
done
printf '%s\n' 'MinIO health endpoint did not become ready.'
exit 1
