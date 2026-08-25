#!/bin/bash
set -euo pipefail

identity_name="${LIFT_CODESIGN_IDENTITY:-Lift Local Code Signing}"
install_dir="${LIFT_INSTALL_DIR:-${HOME}/bin}"
service_label="git.acsandmann.rift"
lift_identifier="git.acsandmann.rift"
cli_identifier="git.acsandmann.rift.cli"

if ! security find-identity -v -p codesigning \
    | grep -Fq "\"$identity_name\""; then
    echo "Missing code-signing identity: $identity_name" >&2
    echo "Run scripts/setup-local-signing.sh once before installing Lift." >&2
    exit 1
fi

cargo build --release --bin lift --bin lift-cli
codesign --force --timestamp=none --sign "$identity_name" \
    --identifier "$lift_identifier" target/release/lift
codesign --force --timestamp=none --sign "$identity_name" \
    --identifier "$cli_identifier" target/release/lift-cli
codesign --verify --strict --verbose=2 target/release/lift
codesign --verify --strict --verbose=2 target/release/lift-cli

designated_requirement="$(codesign -d -r- target/release/lift 2>&1)"
if grep -Fq 'designated => cdhash' <<<"$designated_requirement"; then
    echo "Refusing to install Lift with a per-build cdhash identity." >&2
    exit 1
fi

mkdir -p "$install_dir"
/usr/bin/install -m 755 target/release/lift "$install_dir/lift.new"
/usr/bin/install -m 755 target/release/lift-cli "$install_dir/lift-cli.new"

if launchctl print "gui/$(id -u)/$service_label" >/dev/null 2>&1; then
    "$install_dir/lift" service stop
fi

mv "$install_dir/lift.new" "$install_dir/lift"
mv "$install_dir/lift-cli.new" "$install_dir/lift-cli"
"$install_dir/lift" service start

echo "$designated_requirement"
launchctl print "gui/$(id -u)/$service_label" \
    | grep -E 'state =|program =|pid =|runs =|last exit code'
