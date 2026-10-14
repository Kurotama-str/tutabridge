#!/usr/bin/env python3
"""Prepare Tauri payloads, validate signatures and publish a complete release."""
import argparse
import base64
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile

REPO = 'spartanz51/tutabridge'
PAYLOADS = {
    'darwin-aarch64': 'TutaBridge-macOS.app.tar.gz',
    'darwin-x86_64': 'TutaBridge-macOS.app.tar.gz',
    'linux-x86_64': 'TutaBridge-Linux.AppImage',
    'windows-x86_64': 'TutaBridge-Windows-setup.exe',
}
ASSETS = set(PAYLOADS.values()) | {
    'TutaBridge-macOS.dmg', 'TutaBridge-Linux.deb', 'TutaBridge-Linux.rpm',
    'TutaBridge-Windows.msi', 'tutabridge-macos-arm64', 'tutabridge-linux-x86_64',
    'tutabridge-windows-x86_64.exe',
}
VERSION = re.compile(r'(\d+\.\d+\.\d+)-rc\.(\d+)\Z')


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()


def app_version(version):
    match = VERSION.fullmatch(version)
    if not match:
        raise ValueError('Expected X.Y.Z-rc.N release version')
    return f'{match[1]}-{match[2]}'


def prepare(args):
    p = Path('src-tauri/tauri.conf.json')
    cfg = json.loads(p.read_text())
    cfg['version'] = app_version(args.version)
    cfg['bundle']['createUpdaterArtifacts'] = False
    p.write_text(json.dumps(cfg, indent=2) + '\n')


def one(pattern):
    matches = list(Path('.').glob(pattern))
    if len(matches) != 1:
        raise ValueError(f'Expected one bundle matching {pattern}, found {len(matches)}')
    return matches[0]


def collect(args):
    dest = Path(args.directory)
    dest.mkdir(parents=True, exist_ok=True)
    bundle = args.bundle
    if args.platform == 'macOS':
        app = one(f'{bundle}/macos/*.app')
        with tarfile.open(dest / 'TutaBridge-macOS.app.tar.gz', 'w:gz') as archive:
            archive.add(app, arcname=app.name)
        files = {'TutaBridge-macOS.dmg': one(f'{bundle}/dmg/*.dmg'),
                 'tutabridge-macos-arm64': Path('target/release/tutabridge')}
    elif args.platform == 'Linux':
        files = {'TutaBridge-Linux.AppImage': one(f'{bundle}/appimage/*.AppImage'),
                 'TutaBridge-Linux.deb': one(f'{bundle}/deb/*.deb'),
                 'TutaBridge-Linux.rpm': one(f'{bundle}/rpm/*.rpm'),
                 'tutabridge-linux-x86_64': Path('target/release/tutabridge')}
    elif args.platform == 'Windows':
        files = {'TutaBridge-Windows-setup.exe': one(f'{bundle}/nsis/*-setup.exe'),
                 'TutaBridge-Windows.msi': one(f'{bundle}/msi/*.msi'),
                 'tutabridge-windows-x86_64.exe': Path('target/release/tutabridge.exe')}
    else:
        raise ValueError('Unknown build platform')
    for name, source in files.items():
        if source.is_symlink() or not source.is_file() or source.stat().st_size == 0:
            raise ValueError(f'Missing or invalid asset: {name}')
        shutil.copy2(source, dest / name)


def require_assets(directory, signed=False):
    expected = ASSETS | ({p + '.sig' for p in PAYLOADS.values()} if signed else set())
    for name in expected:
        p = directory / name
        if p.is_symlink() or not p.is_file() or p.stat().st_size == 0:
            raise ValueError(f'Missing or invalid asset: {name}')
    allowed = expected | ({'latest.json', 'SHA256SUMS'} if signed else set())
    if set(p.name for p in directory.iterdir()) - allowed:
        raise ValueError('Unexpected release asset')


def verify_signature(payload, signature, pubkey):
    with tempfile.TemporaryDirectory() as scratch:
        key = Path(scratch) / 'key.pub'
        sig = Path(scratch) / 'payload.sig'
        key.write_bytes(base64.b64decode(pubkey.strip(), validate=True))
        sig.write_bytes(base64.b64decode(signature.strip(), validate=True))
        run('minisign', '-Vm', str(payload), '-p', str(key), '-x', str(sig))


def manifest(directory, version, pubkey):
    require_assets(directory, signed=True)
    platforms = {}
    for target, name in PAYLOADS.items():
        signature = (directory / (name + '.sig')).read_text().strip()
        verify_signature(directory / name, signature, pubkey)
        platforms[target] = dict(signature=signature, url=f'https://github.com/{REPO}/releases/download/v{version}/{name}')
    return dict(version=app_version(version), notes=f'TutaBridge {version}',
                pub_date=datetime.now(timezone.utc).isoformat(), platforms=platforms)


def sign(args):
    directory = Path(args.directory)
    require_assets(directory)
    for name in sorted(set(PAYLOADS.values())):
        # Signing tools never execute a downloaded payload.
        run('tauri', 'signer', 'sign', str(directory / name))
    pubkey = Path(args.pubkey).read_text().strip()
    data = manifest(directory, args.version, pubkey)
    (directory / 'latest.json').write_text(json.dumps(data, indent=2) + '\n')
    checksums = []
    for p in sorted(directory.iterdir()):
        checksums.append(f'{hashlib.file_digest(p.open("rb"), "sha256").hexdigest()}  {p.name}')
    (directory / 'SHA256SUMS').write_text('\n'.join(checksums) + '\n')


def api(path, method='GET', data=None):
    args = ['gh', 'api', f'repos/{REPO}/{path}']
    if method != 'GET':
        args += ['--method', method]
    if data is not None:
        args += ['--input', '-']
    value = run(*args, input=json.dumps(data) if data is not None else None)
    return json.loads(value) if value else None


def release_state(version, source):
    if not re.fullmatch('[0-9a-f]{40}', source):
        raise ValueError('Release source must be a full SHA')
    tag = 'v' + version
    refs = run('git', 'ls-remote', 'origin', f'refs/tags/{tag}', f'refs/tags/{tag}^{{}}').splitlines()
    if refs:
        commits = {ref: sha for sha, ref in (line.split() for line in refs)}
        resolved = commits.get(f'refs/tags/{tag}^{{}}', commits.get(f'refs/tags/{tag}'))
        if resolved != source:
            raise ValueError('Existing release tag belongs to another source')
    pages = json.loads(run('gh', 'api', '--paginate', '--slurp', f'repos/{REPO}/releases?per_page=100'))
    release = next((r for page in pages for r in page if r['tag_name'] == tag), None)
    if release and release['draft'] and release['target_commitish'] != source:
        raise ValueError('Existing draft belongs to another source; investigate without deleting it')
    if release and not refs:
        raise ValueError('Release exists without its expected tag')
    return release, bool(refs)


def publish(args):
    directory = Path(args.directory)
    pubkey = Path(args.pubkey).read_text().strip()
    expected = manifest(directory, args.version, pubkey)
    actual = json.loads((directory / 'latest.json').read_text())
    if actual['version'] != expected['version'] or actual['platforms'] != expected['platforms']:
        raise ValueError('Updater manifest does not match the signed payloads')
    # Publishing from an old main commit supports a new-version rollback.
    run('git', 'merge-base', '--is-ancestor', args.source, 'origin/main')
    existing, tagged = release_state(args.version, args.source)
    if existing and not existing['draft']:
        print('Release already published; leaving it unchanged')
        return
    # Never move the updater latest pointer backwards to an older rc.
    pages = json.loads(run('gh', 'api', '--paginate', '--slurp', f'repos/{REPO}/releases?per_page=100'))
    current = VERSION.fullmatch(args.version)
    for release in (r for page in pages for r in page if not r['draft']):
        other = VERSION.fullmatch(release['tag_name'].removeprefix('v'))
        if other and (tuple(map(int, other[1].split('.'))), int(other[2])) >= (tuple(map(int, current[1].split('.'))), int(current[2])):
            raise ValueError('Refusing to publish below an existing release version')
    tag = 'v' + args.version
    if not tagged:
        api('git/refs', 'POST', {'ref': f'refs/tags/{tag}', 'sha': args.source})
    if not existing:
        existing = api('releases', 'POST', dict(tag_name=tag, target_commitish=args.source,
            name=f'TutaBridge {args.version}', body=f'Built and tested from `{args.source}`. Includes the patched official Tuta SDK pinned in this commit.', draft=True, prerelease=False))
    run('gh', 'release', 'upload', tag, *[str(p) for p in sorted(directory.iterdir())], '--clobber', '--repo', REPO)
    uploaded = api(f"releases/{existing['id']}")
    names = {p.name: p.stat().st_size for p in directory.iterdir()}
    remote = {asset['name']: asset['size'] for asset in uploaded['assets']}
    if names != remote:
        raise ValueError('Draft assets are incomplete or unexpected; keeping draft unpublished')
    # Check bytes, not only asset sizes, before making the draft public.
    with tempfile.TemporaryDirectory() as downloaded:
        run('gh', 'release', 'download', tag, '--repo', REPO, '--dir', downloaded)
        for name in names:
            if hashlib.sha256((directory / name).read_bytes()).digest() != hashlib.sha256((Path(downloaded) / name).read_bytes()).digest():
                raise ValueError('Uploaded asset differs from signed output')
    api(f"releases/{existing['id']}", 'PATCH', dict(draft=False, prerelease=False, make_latest='true'))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['prepare', 'collect', 'sign', 'publish'])
    parser.add_argument('--version', default='0.1.0-rc.0')
    parser.add_argument('--directory', default='release-assets')
    parser.add_argument('--bundle', default='target/release/bundle')
    parser.add_argument('--platform')
    parser.add_argument('--pubkey')
    parser.add_argument('--source')
    args = parser.parse_args()
    app_version(args.version)
    globals()[args.command](args)
