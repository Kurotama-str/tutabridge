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

The image uses upstream's entrypoint (`tini`, volume lock, `init` / `run`),
with one difference: when `TUTABRIDGE_SESSION_STORE=file` (the image default)
the GPG and `pass` checks are skipped. The Tuta session is kept in
`session.json`, optionally encrypted with a passphrase.

## Build

```bash
git submodule update --init tuta-repo
python3 scripts/sdk-prepare.py
docker build -f 13/Dockerfile -t kurotamastr/tutabridge:local .
```

`--build-arg UID=1000` changes the default user baked into the image. The
`user:` key of a compose file overrides it at run time as well.

## New install

Copy `kurotama_compose.yml`, replace the placeholders, and make the data
directory writable by the container user (10001 by default):

```bash
mkdir -p /path/to/tutabridge/data && sudo chown 10001:10001 /path/to/tutabridge/data
docker compose -f kurotama_compose.yml run --rm -it tutabridge init
docker compose -f kurotama_compose.yml up -d
```

`init` asks for your Tuta address, password and TOTP code, saves the session and
exits. `run` (the default command) never prompts: with no usable session it
stops with a message asking you to run `init`. Stop the service before running
`init`; both use the same volume lock.

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
`docker compose run --rm -it tutabridge hash-password`. Once the bridge password is
hashed, `tutabridge password` can no longer show it: keep it in your mail client
or password manager.

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

* `run` is non-interactive. If the session has expired, the container stops with
  a message instead of prompting. Run
  `docker compose stop && docker compose run --rm -it tutabridge init`, then
  `docker compose up -d`.
* Two new things appear in the config directory: `.lock` (volume lock) and
  `cache/` (the SDK cache, previously lost on recreation).
* The container runs under `tini` and has 20 seconds' grace to stop if you set
  `stop_grace_period`.

To go back, restore the previous image and compose file. The data was not
changed by this step.

### Step 2: optional, align with upstream's layout

Do this to end up on `/data` and UID 10001 like a new install, still with the
file session. Each point is independent; stop at any of them.

1. Stop the container and copy the directory: `cp -a /path/to/tutabridge /path/to/tutabridge.bak`.
2. Move the data into the new layout. The old config directory is the
   `tutabridge` folder inside your mount:

   ```bash
   mkdir -p /path/to/data/config
   mv /path/to/tutabridge/tutabridge /path/to/data/config/tutabridge
   ```

   The `certs/` folder can stay where it is, since it is mounted separately.
3. Change ownership, or keep UID 1000 with `user:` (the container then needs no
   `chown`):

   ```bash
   sudo chown -R 10001:10001 /path/to/data
   ```

4. Switch to the new-install compose file (`kurotama_compose.yml`): mount
   `/path/to/data` at `/data`, mount the certificates at
   `/data/config/tutabridge/cert.pem` and `key.pem`, and drop `user:` and the
   `environment:` block. Keep the unlock page port if you use `web`.
5. Optional hardening already in `kurotama_compose.yml`: `cap_drop: ALL`,
   `no-new-privileges`, `stop_grace_period`. `stdin_open` and `tty` are not
   needed.
6. Optional: rename `bind_address` to `bind_addr` in `config.toml`. The old
   name keeps working.
7. Start, check the logs as in step 1, and keep the `.bak` copy until you are
   sure.

To go back from step 2, stop the container, move
`/path/to/data/config/tutabridge` back to `/path/to/tutabridge/tutabridge`,
restore ownership with `chown -R 1000:1000`, and use the step 1 compose file.

### Moving to upstream's `pass` storage

Not part of this migration: it needs a new login, and the passphrase encryption
here does not apply to it. Use upstream's `compose.yaml` and `docs/DOCKER.md`
with a new volume, and run `init` there.
