# Docker, Kurotama image

This fork ships its own image next to upstream's. Upstream's files
(`Dockerfile`, `compose.yaml`, `docs/DOCKER.md`, `.github/workflows/docker.yml`)
are untouched and describe the `pass` + GPG setup. Everything here is separate:

| File | Purpose |
|---|---|
| `13/Dockerfile` | Debian 13 (trixie) build of the same CLI image |
| `.github/workflows/kurotama_docker.yml` | Tests, then pushes `kurotamastr/tutabridge` as `latest`, a date tag and a `sha` tag |
| `kurotama_compose.yml` | Compose file for a new install |
| `docker_oldbuild_compat/compose.yml` | Compose file for an install that predates this image |
| this file | Differences from upstream, settings, migration |

The image uses upstream's launcher and service supervisor (`tini`, then
`entrypoint.sh`: no argument or `run` becomes `tutabridge run --wait`, anything
else is passed through). The supervisor takes the volume lock, waits for a
session, runs the bridge, retries after failures and answers `health`. The image
sets `TUTABRIDGE_CONTAINER=1`, which turns it on. One difference: when
`TUTABRIDGE_SESSION_STORE=file` (the image default) the GPG and `pass` setup is
skipped. The Tuta session is kept in `session.json`, optionally encrypted with
a passphrase.

Images built from this fork are always `kurotamastr/tutabridge`.

## Build

```bash
cd /path/to/tutabridge-repo
git submodule update --init --recursive
python3 scripts/sdk-prepare.py
docker build -f 13/Dockerfile -t kurotamastr/tutabridge:dev .
docker push kurotamastr/tutabridge:dev   # when you are ready to publish it
```

`-f 13/Dockerfile` is needed because a plain `docker build .` uses upstream's
`Dockerfile` at the root. `:dev` leaves `:latest` untouched.

`--build-arg UID=1000` changes the default user baked into the image. The
`user:` key of a compose file overrides it at run time as well.

## New install

In this document, `APPDIR` is the directory on the host that you mount at
`/data` (the `/path/to/tutabridge/data` line in `kurotama_compose.yml`), for
example `/path/to/APPDIR`. The app creates its own `config/tutabridge/` folder
inside it; do not confuse that with `APPDIR` itself.

Copy `kurotama_compose.yml`, replace the placeholders, and make `APPDIR`
writable by the container user (10001 by default):

```bash
mkdir -p /path/to/APPDIR && sudo chown 10001:10001 /path/to/APPDIR
docker compose -f kurotama_compose.yml up -d
docker exec -it tutabridge init
```

Without Compose, the same with plain Docker:

```bash
docker run -d --name tutabridge --restart unless-stopped \
  -p 127.0.0.1:1143:1143 -p 127.0.0.1:1025:1025 \
  -v /path/to/APPDIR:/data kurotamastr/tutabridge:latest
docker exec -it tutabridge init
```

`init` asks for your Tuta address, password and TOTP code, saves the session and
exits. `run` (the default command) never prompts: with no usable session it
waits, reports itself unhealthy (see Health) and starts IMAP and SMTP within
seconds after `init` completes. So you can start the container before the first
login, and run `init` in it with `docker exec` while it waits.

To log in before starting the service, run `init` in a throwaway container
instead:

```bash
docker run --rm -it -v /path/to/APPDIR:/data kurotamastr/tutabridge:latest init
```

Both use the same volume lock, so do not run this while a service is serving
from the same `APPDIR`; stop it first. `docker exec -it tutabridge password`
prints the local bridge password.

## Layout

| Path in the container | Content |
|---|---|
| `/data/config/tutabridge/` | `config.toml`, `session.json`, TLS files, `blacklist.txt`, `auth_failures.log`, `store.db`, `mails/` |
| `/data/cache/` | SDK cache |
| `/data/.lock` | Held while `init` or `run` is active |

## Settings

| Setting | How | Default |
|---|---|---|
| Listen address (IMAP, SMTP, unlock page) | `TUTABRIDGE_BIND_ADDRESS` env, else `bind_addr` in `config.toml` (`bind_address` is accepted as an alias) | `127.0.0.1`; the image sets the env to `0.0.0.0` |
| Session in a file | `TUTABRIDGE_SESSION_STORE=file` (takes precedence over `pass_path`) | Set by the image |
| Session encryption | `session_encryption_method` = `none`, `file` or `web`, with `session_encryption_key` | `none` |
| Unlock page port (`web`) | `session_unlock_port` | 8080 |
| Unlock file (`file`) | `session_unlock_file` | `<config dir>/unlock` |
| Bridge password | `bridge_password` in `config.toml`: plaintext, or an Argon2id hash | Generated, plaintext |

A hashed value for `bridge_password` or `session_encryption_key` comes from
`docker run --rm -it kurotamastr/tutabridge:latest hash-password` (it needs no
volume), or `docker compose run --rm -it tutabridge hash-password`. Once the
bridge password is hashed, `tutabridge password` can no longer show it and says
so: keep it in your mail client or password manager.

An invalid `TUTABRIDGE_BIND_ADDRESS` is ignored with a warning, and
`bind_addr` is used instead.

### Encrypted session

With `file` or `web`, the bridge waits at every start for the passphrase before
reading `session.json`, also under `run --non-interactive`.

* `web`: open `https://<host>:<session_unlock_port>` and enter it. Publish
  that port in compose.
* `file`: write the passphrase to the unlock file. The bridge deletes it once it
  is accepted.

A wrong passphrase is retried with a growing delay. An encrypted session file
that cannot be opened is never overwritten.

While it waits, the container logs where to unlock and reports itself unhealthy
with a matching reason (see Health). After every restart this stays so until
someone unlocks it.

## Health

The image declares a health check, `tutabridge health`. It opens a TLS
connection to the IMAP and SMTP ports, pinned to the bridge's own certificate,
and checks their greetings without logging in. Docker health has no values
between healthy and unhealthy, so anything that stops the bridge from serving
shows as unhealthy, with the reason as the check's output:

```bash
docker ps
docker inspect --format '{{json .State.Health}}' tutabridge
docker exec tutabridge tutabridge health
```

| Reason | What to do |
|---|---|
| Waiting for session unlock (web page on port N) | Open the unlock page and enter the passphrase. |
| Waiting for session unlock (unlock file PATH) | Write the passphrase to that file. |
| Waiting for configuration / for a saved session | Run `docker exec -it tutabridge init`. |
| Saved session rejected | Tuta no longer accepts it (a password change, for example). Run `init` again; the old session is kept until a new login succeeds. |

The other states (version rejected, Tuta unavailable, storage, volume occupied)
are the same as upstream's, see the status table in `docs/DOCKER.md`. Docker
shows `starting` during the first 30 s (the start period) before it reports
healthy or unhealthy.

## Exposing the ports

Publishing the IMAP and SMTP ports beyond the host (a LAN, a reverse proxy, the
internet) is done at your own risk. Whoever can reach them can try to log in,
and the bridge password is the only protection. Mitigations, none of them a
guarantee:

* Publish only on the interfaces you need (`127.0.0.1:port:1143` rather than
  `port:1143`), and filter with a firewall.
* Use a long random bridge password, and store it hashed (below).
* Keep the lockout and the blacklist in mind: they count the address the
  bridge sees, see the next section.
* Use a certificate your clients can verify (see Certificate) rather than the
  self-signed one.
* Encrypt the session file (`session_encryption_method`), since the bridge
  holds a live Tuta session.

At startup the bridge logs these notices:

| Condition | Level |
|---|---|
| Listening on a non-loopback address (always the case inside a container, so look at what you publish) | warning |
| `bridge_password` is plaintext while listening beyond loopback | warning, with the `hash-password` hint |
| `session.json` is not encrypted (file store, method `none`) | info, a warning when listening beyond loopback |

## Failed logins, lockouts, blacklist

Failed IMAP and SMTP logins are counted per client IP. The first three are free;
each further failure locks that IP out one minute longer than the last, up to
five minutes. Counts are kept in memory and reset on restart.

Both files are in the config directory:

* `auth_failures.log`: one line per failure past the free three, with time,
  protocol, IP, failure count and lockout length.
* `blacklist.txt`: one IP per line (`#` for comments). Listed IPs are dropped
  before the TLS handshake. Edits apply without a restart.

The bridge sees the TCP peer address. Behind a proxy that does not pass the
client address on, every client shares one IP, so a lockout or blacklist entry
applies to all of them. The IP in `auth_failures.log` shows what the bridge
sees.

## Certificate

By default the bridge makes a self-signed certificate valid for `localhost`
only. To use a real one, mount the chain as `cert.pem` and the key as
`key.pem` in the config directory (see the commented lines in
`kurotama_compose.yml`) and restart after each renewal. PKCS#8, PKCS#1 and SEC1
keys are accepted.

If either file is present but empty as the bridge sees it, TLS setup fails and
the log names the file. Empty files next to a working bind mount, on the
host, are only mount points and cause no problem.

## Migrating an existing installation

This covers an install that runs the older `kurotamastr/tutabridge` image with
the session in `session.json`: config mounted at `/config`, user `1000:1000`,
`TUTABRIDGE_SESSION_STORE=file` set by the image. The session file stays in use
throughout, and no new login is needed.

### Step 1: minimal, keep everything as it is

Only your compose file changes. Compare it with
`docker_oldbuild_compat/compose.yml` and apply three things:

1. `image:` stays `kurotamastr/tutabridge:latest`, which now comes from
   `13/Dockerfile`.
2. Keep `user: "1000:1000"`, or whichever UID owns your config directory.
3. Add the `environment:` block, which points the image's `/data` defaults back
   to your `/config` mount:

   ```yaml
   environment:
     TUTABRIDGE_DATA_DIR: /config
     XDG_CONFIG_HOME: /config
     XDG_CACHE_HOME: /config/cache
     GNUPGHOME: /config/gnupg
     PASSWORD_STORE_DIR: /config/pass
   ```

Mounts, ports, network alias, certificates and `config.toml` stay as they are.
Before pulling, note the digest of the image you run now
(`docker image inspect --format '{{index .RepoDigests 0}}' kurotamastr/tutabridge:latest`),
or use an older date tag, so you can go back.

```bash
docker compose pull && docker compose up -d && docker compose logs -f
```

Expect, in order: the unlock prompt if you use `web` or `file` encryption,
`Session unlocked`, `Logged in as ...`, then `Bridge is running`.

What differs from the old image, even in this step:

* `run` is non-interactive. If the session has expired, the container keeps
  running, reports itself unhealthy and waits instead of prompting. Run
  `docker exec -it tutabridge init`; it starts serving by itself within seconds.
* Two new things appear in the config directory: `.lock` (volume lock) and
  `cache/` (the SDK cache, previously lost on recreation).
* The container runs under `tini` and has 20 seconds' grace to stop if you set
  `stop_grace_period`.

To go back, restore the previous image and compose file. The data was not
changed by this step.

### Step 2: optional, align with upstream's layout

Do this to end up on `/data` and UID 10001 like a new install, still with the
file session. Each point is independent; stop at any of them.

In this section, `APPDIR` is, as above, a directory on the host, here the one
you mounted at `/config` in your old compose file, for example
`/path/to/APPDIR`. The old image created a `tutabridge/` folder in it, holding `config.toml`,
`session.json`, `store.db` and `mails/`.

The new layout expects `APPDIR` mounted at `/data`, with the config folder at
`APPDIR/config/tutabridge/`. So the only move is `APPDIR/tutabridge` to
`APPDIR/config/tutabridge`. `APPDIR/cache` is already where the new layout wants
it, and `certs/` can stay where it is.

1. Stop the container and copy the directory: `cp -a /path/to/APPDIR /path/to/APPDIR.bak`.
2. Move the config folder:

   ```bash
   cd /path/to/APPDIR
   mkdir config
   mv tutabridge config/tutabridge
   ```

   If you keep a symlink such as `config.toml -> tutabridge/config.toml` in
   `APPDIR`, point it at `config/tutabridge/config.toml`.
3. Change ownership of `APPDIR` (certificates inside it included), or keep UID
   1000 with `user:` and skip this:

   ```bash
   sudo chown -R 10001:10001 /path/to/APPDIR
   ```

   Files outside `APPDIR` that the container uses must stay reachable by the new
   user, for example the mail backup directory and certificates stored
   elsewhere. An ACL avoids changing their owner:
   `sudo setfacl -R -m u:10001:rwX /path/to/mail/backup`.
4. Switch the compose file to the new layout (`kurotama_compose.yml`): mount
   `APPDIR` at `/data`, the certificates at
   `/data/config/tutabridge/cert.pem` and `key.pem`, and drop `user:` and the
   `environment:` block. Keep the unlock page port if you use `web`.
5. Optional hardening already in `kurotama_compose.yml`: `cap_drop: ALL`,
   `no-new-privileges`, `stop_grace_period`. `stdin_open` and `tty` are not
   needed.
6. Optional: rename `bind_address` to `bind_addr` in `config.toml`. The old
   name keeps working.
7. Start, check the logs as in step 1, and keep the `.bak` copy until you are
   sure.

Result:

```text
APPDIR/                  mounted at /data
  cache/
  certs/                 yours, mounted separately into the container
  config/tutabridge/     config.toml, session.json, store.db, mails/, ...
```

Without moving anything: mount `APPDIR` at `/data` and set
`XDG_CONFIG_HOME: /data` in `environment:`. The bridge then reads
`APPDIR/tutabridge/`, where the files already are. This keeps the `/data` mount
and the new user, and differs from a stock layout only in the folder name.

To go back from step 2, stop the container, run
`mv config/tutabridge tutabridge` in `APPDIR`, restore ownership with
`chown -R 1000:1000`, and use the step 1 compose file.

### Moving to upstream's `pass` storage

Not part of this migration: it needs a new login, and the passphrase encryption
here does not apply to it. Use upstream's `compose.yaml` and `docs/DOCKER.md`
with a new volume, and run `init` there.
