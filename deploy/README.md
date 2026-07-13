# Production deployment

This deployment runs a built, immutable release rather than `cargo run`. The
paths below are defaults; change the unit and commands together if the host uses
a different layout.

## Build and promote a release

Build from the exact reviewed commit on a build machine:

```sh
cd app
corepack enable
npm ci
npm run build
cd ..
cargo build --locked --release -p hirsel-host
```

Create a new versioned directory on the host instead of overwriting a running
binary. Replace `<release-id>` with the tag or full commit SHA:

```sh
sudo install -d -o root -g root /opt/hirsel/releases/<release-id>/app
sudo install -m 0755 target/release/hirsel-host \
  /opt/hirsel/releases/<release-id>/hirsel-host
sudo cp -a app/dist /opt/hirsel/releases/<release-id>/app/
sudo cp -a templates /opt/hirsel/releases/<release-id>/
sudo ln -sfn /opt/hirsel/releases/<release-id> /opt/hirsel/current.new
sudo mv -Tf /opt/hirsel/current.new /opt/hirsel/current
```

The versioned directory is immutable after promotion. `HIRSEL_APP_DIR` below
and the unit's `HIRSEL_TEMPLATES_DIR` both follow the atomically replaced
`current` symlink to the matching web build and view-template catalog.

## One-time service setup

Create a dedicated system account, data directory, and root-owned environment
file:

```sh
sudo useradd --system --home-dir /var/lib/hirsel --shell /usr/sbin/nologin hirsel
sudo install -d -o hirsel -g hirsel -m 0700 /var/lib/hirsel
sudo install -d -o root -g root -m 0755 /etc/hirsel
sudo install -o root -g hirsel -m 0640 /dev/null /etc/hirsel/hirsel.env
```

Populate `/etc/hirsel/hirsel.env` without committing it:

```ini
HIRSEL_LISTEN=127.0.0.1:3089
HIRSEL_APP_DIR=/opt/hirsel/current/app/dist
HIRSEL_TOKEN=<long-random-token>
HIRSEL_PROVIDER=<provider>
# Add the selected provider's credentials and model here.
```

`HIRSEL_DATA_DIR` is fixed by the unit at `/var/lib/hirsel`. Install and enable
the unit only after a release has been promoted:

```sh
sudo install -o root -g root -m 0644 deploy/hirsel-host.service \
  /etc/systemd/system/hirsel-host.service
sudo systemctl daemon-reload
sudo systemctl enable hirsel-host.service
sudo systemctl start hirsel-host.service
```

## One-time data migration and cutover

The operator performs the cutover. Stop the old development process first so
SQLite and related files cannot change while they are copied. Back up both the
old and new paths, then copy the contents of the old shared-build-volume data
directory into the dedicated directory while preserving metadata:

```sh
sudo rsync -aHAX --numeric-ids <old-data-dir>/ /var/lib/hirsel/
sudo chown -R hirsel:hirsel /var/lib/hirsel
sudo chmod 0700 /var/lib/hirsel
```

Verify the copied data before starting the unit. After startup, check
`systemctl status hirsel-host.service` and `journalctl -u hirsel-host.service`.
Keep the backup until application behavior and persistence have been verified.

For subsequent releases, build and promote another versioned directory, switch
the `current` symlink atomically, and restart `hirsel-host.service`. Rollback
switches the symlink to the prior directory and restarts the unit; data-schema
compatibility must be assessed before rollback.

This service replaces the `just dev`/`entr` loop in production. That loop is
still appropriate for local development, but it runs from a mutable checkout
and restarts on source changes; production executes only the promoted release.
