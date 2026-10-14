# Vendored SDK

The bridge builds against Tuta's official Rust SDK, not a hand-maintained
fork. `tuta-repo` is **generated**: the official release named in
`sdk/BASE`, plus the patches in `sdk/patches/`, applied in order. Nothing in
the generated tree is edited by hand.

```
sdk/BASE                  official release tag + its commit
sdk/patches/NN-*.patch    our changes, one concern each
scripts/sdk-generate.sh   applies them, reproducibly
tuta-repo                 submodule pinned at the generated commit
crates/tuta               bridge-specific code that used to live in the fork
```

The result is hosted on the SDK fork (`spartanz51/tutanota`, one branch per
generated commit, `generated/<version>-<commit>`, never rewritten) only so
that `cargo`, CI, `dev.sh` and the AUR `-git` package keep building from a
plain submodule. The fork is an output, not a source.

## Checking a pin

```sh
scripts/sdk-generate.sh --check
```

Regenerates the SDK from `sdk/BASE` and `sdk/patches/` and fails unless the
pinned `tuta-repo` commit is exactly that. Generation is reproducible (fixed
committer, committer date taken from each patch), so the same inputs always
give the same commit. It also fails if the patches touch the SDK's
`Cargo.toml`: the client version Tuta checks must be the release's own,
never an edit. CI runs this on every pull request.

```sh
scripts/sdk-generate.sh --verify-each
```

Also checks every patch on its own: after each one, the SDK must be
formatted and pass its tests. Slower; run it whenever a
patch changes.

## Moving to a new Tuta release

The automated path is described in **Automatic releases** below. Ordinary
bumps require no manual SDK edit or tag. If verification fails, fix or remove
the affected patch in `sdk/patches`, test it with
`scripts/sdk-generate.sh --verify-each`, and review that correction before
rerunning the workflow. Do not edit the generated submodule by hand.

Tuta raises the minimum client version it accepts every few months (HTTP
474 below it), so staying on recent releases is what keeps the bridge able
to log in.

## The patches

Each patch is one concern, formatted, and builds and passes the SDK tests
on its own, with a few focused tests. Patches that could go to Tuta come
first, internal ones last; which is which, and their status, is tracked in
[`SDK_UPSTREAM.md`](SDK_UPSTREAM.md).

| Patch | Adds | Why it lives in the SDK |
|---|---|---|
| 01 optional empty | An empty optional encrypted value is null, as in TS `CryptoMapper` | Protocol fix. |
| 02 normalize address | `create_session` trims and lowercases the address for the salt and the session, as TS does | Fix in the SDK's own login. |
| 03 account KDF | The passphrase key uses the KDF the salt service reports; Bcrypt is an error instead of a panic | Fix in the SDK's own login. |
| 04 load multiple | `load_multiple`: list elements by id, 100 per request | Transport and parsing are partly private; avoids one request per mail. |
| 05 blob downloads | `download_blobs`: instance-scoped read tokens, 100 blobs per request, retry and failover | Reuses the SDK's token service and blob servers. Attachments. |
| 06 blob elements | `load_blob_element`: a blob element (`MailDetailsBlob`) from its archive | Same pipeline as 05. Mail bodies. |
| 07 parse raw | `parse_raw`: a raw JSON entity through the serializer | Inline event payloads and blob contents use the official parser. |
| 08 interactive session | `initiate_session` and the TOTP second-factor calls | Reuses services, key derivation and session bootstrap. |
| 09 owner session key | `decrypt_parsed` with a session key from the owning instance | Blob elements and draft details have no key of their own. |
| 10 AEAD session reads | Decrypts AEAD v3 values with the session key and field context | Hooks the existing primitives into the entity decoder. |
| 11 AEAD group reads | Decrypts AEAD v2 values with versioned group keys and the KDF nonce | Key resolution belongs with the key loader. |
| 12 bucket instance keys | Retains keys for attachment children from their parent Mail | Decapsulation and key context remain in the SDK. |

What the fork used to carry beyond this (event bus client, folder tree,
MOVE, `MailSetEntry` id codec, inline event decryption) composes from the
SDK's public API and now lives in `crates/tuta`.

AEAD **writes** are not included: they are prototyped on the
`lab/sdk-official` branch but not validated against a real account.

## History

Until September 2026 the SDK was a fork of tutao's 348 release carrying
twelve bridge commits. Its version string was raised to 359 to clear Tuta's
version floor (#38), then @ninjapanzer rebased the commits onto the real
359 release (#47) and documented why a relabelled version is not a real
one. This layout replaces the fork with generated releases, so a new Tuta
release is a new tag in `sdk/BASE`, a regeneration and a test run.


## Automatic releases

Every other Monday at 06:00 UTC (anchor: 2026-10-19), `sdk-bump.yml`
selects the latest official Tuta release tag. The weekly cron exits on the
alternate week before querying Tuta. An unfinished release is resumed on
the next weekly wake-up; the fortnightly gate applies only to new bumps.
A manual run bypasses the date guard.
The GUI checks for published updates every six hours; this is a separate clock.

The workflow tests every patch and resolves the bridge lockfile conservatively
in a job without publication credentials, including during dry runs. It checks
locked Cargo metadata and passes only a bounded lockfile diff and its SHA256
to the publishing job. New dependency sources require review. It regenerates the same SDK commit in a fresh Git-only job, and opens a candidate
PR. Full bridge CI runs against that exact commit. Only the expected SDK pin,
base, submodule branch, `Cargo.lock` and `release-plan.json` may change.
Bridge builds and tests use `--locked`; they cannot repair a stale lock silently. The candidate is
integrated with a normal fast-forward only while main is still its captured
parent. Human changes to candidate branches are never overwritten.

A failed patch produces a diagnostic PR, never a merge or release. If Tuta has
already incorporated a patch, review and remove it from `sdk/patches` before
rerunning. Offline tests do not establish that the live Tuta service accepts
the SDK; there is no live account test in this pipeline.

`release-plan.json` records the next rc version, SDK commits and lockfile SHA256. Its introducing
commit is the immutable release source. If packaging or publication fails,
rerunning SDK bump resumes that plan before considering a newer SDK. Builds
run on Linux, macOS and Windows without signing keys or persisted Git credentials.
A fresh job signs standard updater payloads, verifies them against the app's
public key and publishes one complete `latest.json`. Published releases are
never modified; a conflicting tag or draft stops for investigation.

Secrets: `SDK_BUMP_TOKEN` can write the SDK fork and candidate PRs;
`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` sign updates.
Main/tag/release writes use `GITHUB_TOKEN`, and downstream jobs are invoked
explicitly rather than depending on token pushes to trigger workflows.
Back up the signing key: installed apps trust its public counterpart.

Validation modes: SDK bump with `dry_run=true` only regenerates/tests the SDK,
without any push, PR or release. A manual Release run builds all platforms and
signs with an ephemeral test key, publishing workflow artifacts only. Those
preview signatures are intentionally not trusted by production apps.
For a bridge-only fix or rollback, dispatch Release with `publish=true`, an
explicit main commit and a new higher rc version. This path runs full CI too.
Retry with the same version/source to resume an unpublished draft. Installed
apps never downgrade their version. Editing a committed release plan by hand
invalidates its provenance and stops automation until the plan is repaired.
