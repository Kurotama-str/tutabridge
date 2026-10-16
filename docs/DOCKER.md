# Running TutaBridge in Docker

One container serves both other containers on the same Docker network and
mail clients elsewhere on your LAN (or VPN).

## Build

The build needs the `tuta-repo` submodule:

```bash
git submodule update --init --recursive
docker compose build
```

## First login

The Tuta session is stored in `./data` (a file, not an OS keychain), so sign in
once interactively:

```bash
mkdir -p data && sudo chown 1000:1000 data
docker compose run --rm -it tutabridge
```

Enter your Tuta address, password and TOTP code. When the log says
`Bridge is running`, press Ctrl+C, then start it for good:

```bash
docker compose up -d
```

`docker compose run --rm tutabridge password` prints the bridge password. To
choose your own (for example 64 random hex characters), set `bridge_password`
in `data/tutabridge/config.toml` and restart.

## Settings

| Setting | How | Default |
|---|---|---|
| Listen address | `TUTABRIDGE_BIND_ADDRESS` env or `bind_address` in `config.toml` | `127.0.0.1` (the image sets `0.0.0.0`) |
| Session in a file | `TUTABRIDGE_SESSION_STORE=file` | OS keychain (the image sets `file`) |
| Ports | `imap_port` / `smtp_port` in `config.toml` | 1143 / 1025 |

Published ports (`ports:` in `docker-compose.yml`) decide who can reach the
bridge from outside the container. The MCP server always stays on loopback.

## Failed logins, lockouts, blacklist

Failed IMAP and SMTP logins are counted per client IP. The first three are free;
each further failure locks that IP out one minute longer than the last, up to
five minutes. Counts live in memory and reset on restart.

Both files are in `data/tutabridge/`:

* `auth_failures.log`: one line per failure past the free three, with time,
  protocol, IP, failure count and lockout length.
* `blacklist.txt`: one IP per line (`#` for comments). Listed IPs are dropped
  before the TLS handshake. Edits apply without a restart.

If every client appears with the same address (Docker's gateway), the lockout
and the blacklist apply to all of them at once; check the log for the
addresses you see.

## Certificate

By default the bridge makes a self-signed certificate valid only for
`localhost`. To use a real one, mount the certificate chain as
`/config/tutabridge/cert.pem` and its key as `/config/tutabridge/key.pem` (the
commented lines in `docker-compose.yml`), and restart the container after each
renewal. PKCS#8, PKCS#1 and SEC1 keys are accepted.
