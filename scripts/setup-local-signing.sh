#!/bin/bash
set -euo pipefail

identity_name="${LIFT_CODESIGN_IDENTITY:-Lift Local Code Signing}"
default_keychain="$(
    security default-keychain -d user \
        | sed -E 's/^[[:space:]]*"//; s/"[[:space:]]*$//'
)"
keychain_path="${LIFT_SIGNING_KEYCHAIN:-$default_keychain}"

if security find-identity -v -p codesigning "$keychain_path" \
    | grep -Fq "\"$identity_name\""; then
    echo "Code-signing identity already exists: $identity_name"
    exit 0
fi

temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/lift-signing.XXXXXX")"
cleanup() {
    rm -rf -- "$temp_dir"
}
trap cleanup EXIT

bundle_password="$(openssl rand -hex 24)"
openssl req -new -newkey rsa:2048 -x509 -sha256 -days 3650 -nodes \
    -subj "/CN=$identity_name/O=Lift Local Development" \
    -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,digitalSignature,keyCertSign" \
    -addext "extendedKeyUsage=critical,codeSigning" \
    -keyout "$temp_dir/private-key.pem" \
    -out "$temp_dir/certificate.pem" >/dev/null 2>&1
openssl pkcs12 -export \
    -inkey "$temp_dir/private-key.pem" \
    -in "$temp_dir/certificate.pem" \
    -name "$identity_name" \
    -passout "pass:$bundle_password" \
    -out "$temp_dir/identity.p12"

security import "$temp_dir/identity.p12" \
    -k "$keychain_path" \
    -f pkcs12 \
    -P "$bundle_password" \
    -x \
    -T /usr/bin/codesign >/dev/null
security add-trusted-cert \
    -r trustRoot \
    -p codeSign \
    -k "$keychain_path" \
    "$temp_dir/certificate.pem"

if ! security find-identity -v -p codesigning "$keychain_path" \
    | grep -Fq "\"$identity_name\""; then
    echo "Created certificate, but macOS does not consider it a valid code-signing identity." >&2
    exit 1
fi

echo "Created local code-signing identity: $identity_name"
echo "Its private key is non-extractable and stored in: $keychain_path"
