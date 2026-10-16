#!/usr/bin/env python3
"""Prepare the pinned official SDK plus local patches; never publish SDK code."""
import argparse
import contextlib
import hashlib
import io
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
UPSTREAM = 'https://github.com/tutao/tutanota.git'
EXPORT = ['Cargo.toml', 'LICENSE.txt', 'tuta-sdk/rust',
          'test/tests/api/worker/crypto/CompatibilityTestData.json']
SHA = re.compile(r'[0-9a-f]{40}\Z')
HEX = re.compile(r'[0-9a-f]{64}\Z')


def git_bytes(root, *args, env=None, data=None, checkout_config=False):
    clean = dict(os.environ)
    normalization = []
    if not checkout_config:
        clean.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull)
        normalization = ['-c', 'core.autocrlf=false', '-c', 'core.eol=lf']
    for key in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_CONFIG_COUNT', 'GIT_CONFIG_PARAMETERS'):
        clean.pop(key, None)
    clean.update(env or {})
    return subprocess.check_output(['git', '-C', str(root), *normalization,
        '-c', 'core.hooksPath=' + os.devnull, '-c', 'apply.whitespace=nowarn',
        *args], env=clean, input=data)


def git(root, *args, **kwargs):
    return git_bytes(root, *args, **kwargs).decode().strip()


def inputs(root):
    # tag= is informational here; the release controller validates it.
    commits = [line[7:] for line in (root/'sdk/BASE').read_text().splitlines()
               if line.startswith('commit=')]
    if len(commits) != 1 or not SHA.fullmatch(commits[0]):
        raise ValueError('Invalid sdk/BASE commit')
    directory=root/'sdk/patches'
    if directory.is_symlink() or any(p.is_dir() or p.is_symlink() or not p.is_file() for p in directory.iterdir()):
        raise ValueError('SDK patches must be regular files in one directory')
    patches = sorted((p for p in directory.iterdir() if p.name.endswith('.patch')),
                     key=lambda p: p.name.encode('utf-8'))
    if not patches:
        raise ValueError('No SDK patches')
    result = []
    for p in patches:
        if p.is_symlink() or not p.is_file() or not re.fullmatch(r'[A-Za-z0-9_.-]+', p.name):
            raise ValueError('Invalid SDK patch file')
        result.append((p.name, hashlib.sha256(p.read_bytes()).hexdigest()))
    return commits[0], result


def read_expected(root):
    lines = (root/'sdk/EXPECTED').read_text().splitlines()
    if len(lines) < 4 or not all(lines[i].startswith(k+'=') for i,k in enumerate(['base','tree','content'])):
        raise ValueError('Invalid sdk/EXPECTED')
    base, tree, content = (line.split('=',1)[1] for line in lines[:3])
    patches = []
    for line in lines[3:]:
        if not line.startswith('patch='):
            raise ValueError('Invalid expected patch')
        name, digest = line[6:].split(':',1)
        if not re.fullmatch(r'[A-Za-z0-9_.-]+',name) or not HEX.fullmatch(digest):
            raise ValueError('Invalid expected patch')
        patches.append((name,digest))
    if not SHA.fullmatch(base) or not SHA.fullmatch(tree) or not HEX.fullmatch(content):
        raise ValueError('Invalid expected digest')
    return dict(base=base,tree=tree,content=content,patches=patches)


def write_expected(root, expected):
    text = ''.join(f'{k}={expected[k]}\n' for k in ('base','tree','content'))
    text += ''.join(f'patch={name}:{digest}\n' for name,digest in expected['patches'])
    (root/'sdk/EXPECTED').write_bytes(text.encode('utf-8'))


def content_digest(folder):
    if folder.is_symlink() or not folder.is_dir():
        raise ValueError('SDK output missing; run python3 scripts/sdk-prepare.py')
    h = hashlib.sha256()
    files = []
    for p in folder.rglob('*'):
        if p.is_symlink() or not (p.is_dir() or p.is_file()):
            raise ValueError('SDK output contains a non-regular file')
        if p.is_file():
            files.append(p)
    for p in sorted(files, key=lambda p:p.relative_to(folder).as_posix().encode()):
        name = p.relative_to(folder).as_posix().encode()
        data = p.read_bytes()
        h.update(len(name).to_bytes(8,'big'));h.update(name)
        h.update(len(data).to_bytes(8,'big'));h.update(data)
    return h.hexdigest()


def verify_source(root, base):
    source = root/'tuta-repo'
    if Path(git(source,'rev-parse','--show-toplevel')).resolve() != source.resolve():
        raise ValueError('Official submodule missing; run git submodule update --init tuta-repo')
    if git(source,'rev-parse','HEAD') != base:
        raise ValueError('Official submodule HEAD differs from sdk/BASE')
    entry = git(root,'ls-files','--stage','tuta-repo').split()
    if entry[:2] != ['160000',base] or entry[2:] != ['0','tuta-repo']:
        raise ValueError('Official submodule gitlink differs from sdk/BASE')
    if git(root,'config','-f','.gitmodules','submodule.tuta-repo.url') != UPSTREAM:
        raise ValueError('SDK submodule must reference official Tuta')
    # Compare with the checkout's conversion rules (including system autocrlf).
    status = git(source,'status','--porcelain','--untracked-files=normal',
                 '--ignore-submodules=all',checkout_config=True)
    if status:
        raise ValueError('Official SDK checkout has local changes:\n' + '\n'.join(status.splitlines()[:5]))


def check(root=ROOT):
    expected = read_expected(root)
    base, patches = inputs(root)
    if (base, patches) != (expected['base'],expected['patches']):
        raise ValueError('SDK base or patch series differs from sdk/EXPECTED')
    verify_source(root,base)
    if content_digest(root/'sdk/generated') != expected['content']:
        raise ValueError('Prepared SDK content differs from sdk/EXPECTED; run python3 scripts/sdk-prepare.py')
    return expected


def extract(source, tree, output, paths=EXPORT):
    archive = git_bytes(source,'archive','--format=tar',tree,*paths)
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        for item in tar:
            path = Path(item.name)
            if path.is_absolute() or '..' in path.parts or not (item.isfile() or item.isdir()):
                raise ValueError('Unexpected archive member')
            target = output/path
            if item.isdir():
                target.mkdir(parents=True,exist_ok=True)
            else:
                target.parent.mkdir(parents=True,exist_ok=True)
                target.write_bytes(tar.extractfile(item).read())
                target.chmod(0o755 if item.mode & 0o111 else 0o644)


def generate(root, verify_each=False):
    base, patches = inputs(root)
    verify_source(root,base)
    source=root/'tuta-repo'
    with tempfile.TemporaryDirectory(prefix='tutabridge-sdk-') as scratch, contextlib.ExitStack() as cleanup:
        scratch=Path(scratch)
        env={'GIT_INDEX_FILE':str(scratch/'index')}
        git(source,'read-tree',base,env=env)
        if verify_each:
            work=scratch/'test'
            git(source,'worktree','add','--quiet','--detach',str(work),base)
            cleanup.callback(lambda: git(source,'worktree','remove','--force',str(work)))
        for name,_ in patches:
            p=root/'sdk/patches'/name
            try:
                git(source,'apply','--cached','--whitespace=nowarn',str(p),env=env)
            except subprocess.CalledProcessError:
                try:
                    git(source,'apply','--cached','--check','--reverse',str(p),env=env)
                except subprocess.CalledProcessError:
                    raise ValueError(f'{name} does not apply; review the patch') from None
                raise ValueError(f'{name} is already included upstream; remove it from sdk/patches') from None
            if verify_each:
                tree=git(source,'write-tree',env=env)
                # Full upstream workspace/fixtures are needed by SDK tests only.
                git(work,'read-tree','--reset','-u',tree)
                subprocess.run(['cargo','fmt','--check','-p','tuta-sdk'],cwd=work,check=True)
                target=os.environ.get('CARGO_TARGET_DIR',str(root/'target/sdk-verify'))
                subprocess.run(['cargo','test','--quiet','-p','tuta-sdk'],cwd=work,
                    env=dict(os.environ,CARGO_TARGET_DIR=target),check=True)
                print(f'{name}: formatted, tests pass',flush=True)
        tree=git(source,'write-tree',env=env)
        diff=git(source,'diff',base,tree,'--','*Cargo.toml')
        if re.search(r'^[+-]\s*version\s*=',diff,re.M):
            raise ValueError('SDK patches must not change crate versions')
        return tree


def prepare(root=ROOT, update_expected=False, verify_each=False):
    base, patches=inputs(root)
    if not update_expected:
        expected=read_expected(root)
        if (base,patches)!=(expected['base'],expected['patches']):
            raise ValueError('SDK inputs differ from sdk/EXPECTED; regenerate EXPECTED deliberately')
        if not verify_each:
            try:
                return check(root)
            except (ValueError,FileNotFoundError):
                pass
    tree=generate(root,verify_each)
    if not update_expected and tree!=expected['tree']:
        raise ValueError('Patched SDK tree differs from sdk/EXPECTED')
    folder=root/'sdk/generated'
    if folder.is_symlink():
        raise ValueError('Refusing to replace symlink sdk/generated')
    with tempfile.TemporaryDirectory(prefix='.sdk-stage-',dir=root/'sdk') as temp:
        staging=Path(temp)/'generated';staging.mkdir()
        extract(root/'tuta-repo',tree,staging)
        actual=dict(base=base,tree=tree,content=content_digest(staging),patches=patches)
        if not update_expected and actual!=expected:
            raise ValueError('Extracted SDK differs from sdk/EXPECTED')
        if folder.exists():
            shutil.rmtree(folder)
        staging.rename(folder)
    if update_expected:
        write_expected(root,actual)
    return check(root)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command',nargs='?',choices=['prepare','check'],default='prepare')
    parser.add_argument('--update-expected',action='store_true')
    parser.add_argument('--verify-each',action='store_true')
    args=parser.parse_args()
    try:
        result=check() if args.command=='check' else prepare(update_expected=args.update_expected,verify_each=args.verify_each)
        print('SDK prepared and verified: '+result['tree'])
    except (ValueError,FileNotFoundError,subprocess.CalledProcessError) as error:
        parser.exit(1,f'SDK preparation failed: {error}\n')
