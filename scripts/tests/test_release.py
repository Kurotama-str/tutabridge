import contextlib
from datetime import date, timedelta
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import shutil
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), SCRIPTS / f'{name}.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


sdk = load('sdk-release')
assets = load('release-assets')


@contextlib.contextmanager
def cwd(path):
    original = Path.cwd()
    os.chdir(path)
    try:
        yield
    finally:
        os.chdir(original)


class CalendarAndVersions(unittest.TestCase):
    def test_calendar_crosses_year_without_reset(self):
        start = date(2026, 10, 19)
        for offset in range(400):
            self.assertEqual(sdk.scheduled_today(start + timedelta(days=offset)), offset % 14 == 0)

    def test_numeric_latest_not_lexical(self):
        self.assertGreater(sdk.tag_key('tutanota-release-1000.270101.0'), sdk.tag_key('tutanota-release-999.261231.0'))
        with self.assertRaises(ValueError):
            sdk.tag_key('dev-mail')

    def test_allocate_from_tags_and_drafts(self):
        self.assertEqual(sdk.next_version(['v0.1.0-rc.9', 'v0.1.0-rc.10'], ['v0.1.0-rc.12']), '0.1.0-rc.13')
        with self.assertRaises(ValueError):
            sdk.next_version(['v0.1.0'], [])

    def test_windows_version(self):
        self.assertEqual(assets.app_version('0.1.0-rc.14'), '0.1.0-14')
        with self.assertRaises(ValueError):
            assets.app_version('0.1.0-rc.14;whoami')


class WorkflowArguments(unittest.TestCase):
    def test_actual_detect_step_preserves_dry_run_and_schedule(self):
        workflow = (SCRIPTS.parent / '.github/workflows/sdk-bump.yml').read_text()
        body = workflow.split('      - name: Select SDK or resume pending release', 1)[1].split('        run: |\n', 1)[1]
        lines = []
        for line in body.splitlines():
            if line and not line.startswith('          '):
                break
            lines.append(line[10:])
        script = '\n'.join(lines)
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            executable = root / 'python3'
            executable.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$ARGUMENT_LOG"\n')
            executable.chmod(0o755)
            for dry_run,event,expected in [('true','workflow_dispatch',['--dry-run']),('false','schedule',['--scheduled']),('false','workflow_dispatch',[])]:
                env = dict(os.environ, PATH=str(root)+':'+os.environ['PATH'], ARGUMENT_LOG=str(root/'arguments'), DRY_RUN=dry_run, EVENT_NAME=event, INPUT_TAG='tutanota-release-361.261006.0')
                subprocess.run(['bash','-e','-o','pipefail','-c',script],env=env,check=True)
                args = (root/'arguments').read_text().splitlines()
                self.assertEqual(args, ['scripts/sdk-release.py','detect','--tag',env['INPUT_TAG']]+expected)


class GitCandidate(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.location = Path(self.temp.name)
        self.context = cwd(self.location)
        self.context.__enter__()
        self.addCleanup(self.context.__exit__, None, None, None)
        sdk.git('init', '-q')
        sdk.git('config', 'user.name', sdk.BOT_NAME)
        sdk.git('config', 'user.email', sdk.BOT_EMAIL)
        sdk.git('config', 'commit.gpgSign', 'false')
        Path('sdk').mkdir()
        Path('sdk/BASE').write_text('tag=tutanota-release-360.260922.0\ncommit=' + '1'*40 + '\n')
        Path('.gitmodules').write_text('[submodule "tuta-repo"]\n\tpath = tuta-repo\n\turl = https://github.com/spartanz51/tutanota.git\n\tbranch = old\n')
        sdk.git('add', '.')
        sdk.git('update-index', '--add', '--cacheinfo', f'160000,{"1"*40},tuta-repo')
        sdk.git('commit', '-qm', 'base')
        self.base = sdk.git('rev-parse', 'HEAD')
        sdk.git('branch', '-M', 'main')
        sdk.set_base('tutanota-release-361.261006.0', '2'*40)
        sdk.git('config', '-f', '.gitmodules', 'submodule.tuta-repo.branch', 'generated/361.261006.0-' + '3'*12)
        self.plan = dict(base=self.base, version='0.1.0-rc.14', tag='tutanota-release-361.261006.0', upstream='2'*40, generated='3'*40)
        Path('release-plan.json').write_text(json.dumps(self.plan))
        sdk.git('add', 'sdk/BASE', '.gitmodules', 'release-plan.json')
        sdk.git('update-index', '--cacheinfo', f'160000,{"3"*40},tuta-repo')
        sdk.git('commit', '-qm', 'candidate')
        self.source = sdk.git('rev-parse', 'HEAD')

    def test_accepts_exact_candidate(self):
        self.assertEqual(sdk.validate_candidate(self.source, self.base, '3'*40), self.plan)

    def test_rejects_wrong_base_and_sdk(self):
        for base, generated in [('4'*40, '3'*40), (self.base, '4'*40)]:
            with self.assertRaises(ValueError):
                sdk.validate_candidate(self.source, base, generated)

    def amend(self):
        sdk.git('add', '.')
        sdk.git('update-index', '--add', '--cacheinfo', f'160000,{"3"*40},tuta-repo')
        sdk.git('commit', '--amend', '--no-edit', '-q')
        return sdk.git('rev-parse', 'HEAD')

    def test_unexpected_source_file_cannot_integrate(self):
        Path('malicious.rs').write_text('extra')
        with self.assertRaises(ValueError):
            sdk.validate_candidate(self.amend())

    def test_submodule_url_cannot_change(self):
        sdk.git('config', '-f', '.gitmodules', 'submodule.tuta-repo.url', 'https://example.test/other.git')
        with self.assertRaises(ValueError):
            sdk.validate_candidate(self.amend())

    def test_human_authorship_cannot_integrate(self):
        sdk.git('commit', '--amend', '--no-edit', '--author', 'Human <human@example.test>', '-q')
        with self.assertRaises(ValueError):
            sdk.validate_candidate(sdk.git('rev-parse', 'HEAD'))

    def test_plan_mismatch_cannot_integrate(self):
        self.plan['generated'] = '4'*40
        Path('release-plan.json').write_text(json.dumps(self.plan))
        with self.assertRaises(ValueError):
            sdk.validate_candidate(self.amend())

    def test_main_advance_blocks_push(self):
        real_git = sdk.git
        def git(*args):
            if args[0] == 'ls-remote':
                return '4'*40 + '\trefs/heads/main'
            return real_git(*args)
        with patch.dict(os.environ, SOURCE=self.source, BASE=self.base, GENERATED='3'*40), patch.object(sdk, 'git', side_effect=git), patch.object(sdk, 'push') as push:
            with self.assertRaisesRegex(ValueError, 'Main advanced'):
                sdk.integrate(None)
            push.assert_not_called()

    def test_pending_plan_resumes_exact_source_even_after_main_moves(self):
        Path('notes.txt').write_text('main progressed')
        sdk.git('add', 'notes.txt'); sdk.git('commit', '-qm', 'later')
        args = type('Args', (), dict(scheduled=True, dry_run=False, tag=''))()
        with patch.object(sdk, 'scheduled_today', return_value=False), patch.object(sdk, 'release_list', return_value=[]), patch.object(sdk, 'outputs') as out:
            sdk.detect(args)
        self.assertEqual(out.call_args.kwargs['source'], self.source)
        self.assertEqual(out.call_args.kwargs['mode'], 'resume')

    def test_off_week_does_not_select_a_new_sdk(self):
        args = type('Args', (), dict(scheduled=True, dry_run=False, tag=''))()
        sdk.git('checkout', '--detach', self.base)
        with patch.object(sdk, 'scheduled_today', return_value=False), patch.object(sdk, 'release_list', return_value=[]), patch.object(sdk, 'outputs') as out:
            sdk.detect(args)
            out.assert_called_once_with(mode='skip')


    def test_latest_no_change_dry_run_and_downgrade(self):
        sdk.git('checkout', '--detach', self.base)
        sdk.git('tag', 'v0.1.0-rc.13')
        real_git = sdk.git
        refs = '\n'.join(['1'*40+'\trefs/tags/tutanota-release-360.260922.0', '2'*40+'\trefs/tags/tutanota-release-361.261006.0', '4'*40+'\trefs/tags/tutanota-release-359.260901.0'])
        def git(*args):
            return refs if args[0]=='ls-remote' else real_git(*args)
        for target,dry,mode in [('',False,'bump'),('tutanota-release-360.260922.0',False,'skip'),('tutanota-release-360.260922.0',True,'bump')]:
            args = type('Args',(),dict(scheduled=False,dry_run=dry,tag=target))()
            with patch.object(sdk,'git',side_effect=git),patch.object(sdk,'release_list',return_value=[]),patch.object(sdk,'outputs') as out:
                sdk.detect(args)
                self.assertEqual(out.call_args.kwargs['mode'],mode)
                if not target:
                    self.assertEqual(out.call_args.kwargs['tag'],'tutanota-release-361.261006.0')
        args = type('Args',(),dict(scheduled=False,dry_run=False,tag='tutanota-release-359.260901.0'))()
        with patch.object(sdk,'git',side_effect=git),patch.object(sdk,'release_list',return_value=[]):
            with self.assertRaisesRegex(ValueError,'downgrade'):
                sdk.detect(args)

    def test_verification_failure_only_creates_diagnostic(self):
        sdk.git('checkout', '--detach', self.base)
        real_git = sdk.git
        def git(*args):
            return '' if args[0]=='ls-remote' else real_git(*args)
        env = dict(BASE=self.base,TAG='tutanota-release-361.261006.0',UPSTREAM_SHA='2'*40,VERIFIED='failure',VERSION='0.1.0-rc.14',GITHUB_RUN_ID='123')
        with patch.dict(os.environ,env),patch.object(sdk,'git',side_effect=git),patch.object(sdk,'push') as push,patch.object(sdk,'open_pr') as pr,patch.object(sdk,'outputs') as out:
            sdk.candidate(None)
            self.assertFalse(out.call_args.kwargs['ready'])
            self.assertEqual(out.call_args.kwargs['source'],'')
            self.assertTrue(push.call_args.args[2].endswith('-diagnostic'))
            self.assertIn('never automatically integrated',pr.call_args.args[2])
            self.assertEqual(real_git('diff-tree','--no-commit-id','--name-only','-r','HEAD'),'sdk/BASE')


class Assets(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        for name in assets.ASSETS:
            (self.directory / name).write_bytes(b'fixture')

    def test_complete_assets(self):
        assets.require_assets(self.directory)

    def test_missing_platform_refused(self):
        (self.directory / 'TutaBridge-Windows-setup.exe').unlink()
        with self.assertRaises(ValueError):
            assets.require_assets(self.directory)

    def test_empty_and_symlink_refused(self):
        item = self.directory / 'TutaBridge-Linux.AppImage'
        item.write_bytes(b'')
        with self.assertRaises(ValueError):
            assets.require_assets(self.directory)
        item.unlink()
        item.symlink_to(self.directory / 'TutaBridge-Linux.deb')
        with self.assertRaises(ValueError):
            assets.require_assets(self.directory)

    def test_unexpected_asset_refused(self):
        (self.directory / 'surprise').write_bytes(b'x')
        with self.assertRaises(ValueError):
            assets.require_assets(self.directory)

    def test_manifest_includes_all_four_targets(self):
        for name in set(assets.PAYLOADS.values()):
            (self.directory / (name + '.sig')).write_text('signature')
        with patch.object(assets, 'verify_signature') as verifier:
            result = assets.manifest(self.directory, '0.1.0-rc.14', 'publickey')
        self.assertEqual(set(result['platforms']), set(assets.PAYLOADS))
        self.assertEqual(verifier.call_count, 4)
        self.assertEqual(result['version'], '0.1.0-14')
        for target, payload in result['platforms'].items():
            self.assertTrue(payload['url'].endswith('/v0.1.0-rc.14/' + assets.PAYLOADS[target]))

    def test_signature_failure_stops_manifest(self):
        for name in set(assets.PAYLOADS.values()):
            (self.directory / (name + '.sig')).write_text('signature')
        with patch.object(assets, 'verify_signature', side_effect=ValueError('bad signature')):
            with self.assertRaises(ValueError):
                assets.manifest(self.directory, '0.1.0-rc.14', 'publickey')

    def test_mismatched_tag_or_draft_is_not_deleted(self):
        source = '1'*40
        with patch.object(assets, 'run', return_value='2'*40 + '\trefs/tags/v0.1.0-rc.14'), patch.object(assets, 'api') as api:
            with self.assertRaises(ValueError):
                assets.release_state('0.1.0-rc.14', source)
            api.assert_not_called()
        draft = dict(tag_name='v0.1.0-rc.14', target_commitish='2'*40, draft=True)
        with patch.object(assets, 'run', side_effect=[source + '\trefs/tags/v0.1.0-rc.14', json.dumps([[draft]])]), patch.object(assets, 'api') as api:
            with self.assertRaises(ValueError):
                assets.release_state('0.1.0-rc.14', source)
            api.assert_not_called()


class PublicationRecovery(unittest.TestCase):
    def setUp(self):
        Assets.setUp(self)
        for name in set(assets.PAYLOADS.values()):
            (self.directory / (name + '.sig')).write_text('signature')
        self.key = self.directory.parent / (self.directory.name + '.pub')
        self.key.write_text('public-key')
        self.addCleanup(self.key.unlink)
        self.args = type('Args', (), dict(directory=str(self.directory), version='0.1.0-rc.14', pubkey=str(self.key), source='1'*40))()
        with patch.object(assets, 'verify_signature'):
            self.data = assets.manifest(self.directory, self.args.version, 'public-key')
        (self.directory / 'latest.json').write_text(json.dumps(self.data))
        (self.directory / 'SHA256SUMS').write_text('fixture checksums')

    def test_published_release_is_immutable(self):
        with patch.object(assets, 'verify_signature'), patch.object(assets, 'release_state', return_value=({'draft':False}, True)), patch.object(assets, 'run') as run, patch.object(assets, 'api') as api:
            assets.publish(self.args)
            self.assertEqual(run.call_count, 1)  # ancestry check only
            api.assert_not_called()

    def test_wrong_manifest_version_refused_before_api(self):
        self.data['version'] = '0.1.0-99'
        (self.directory / 'latest.json').write_text(json.dumps(self.data))
        with patch.object(assets, 'verify_signature'), patch.object(assets, 'api') as api:
            with self.assertRaises(ValueError):
                assets.publish(self.args)
            api.assert_not_called()

    def test_wrong_manifest_url_refused_before_api(self):
        self.data['platforms']['linux-x86_64']['url'] = 'https://other.test/file'
        (self.directory / 'latest.json').write_text(json.dumps(self.data))
        with patch.object(assets, 'verify_signature'), patch.object(assets, 'api') as api:
            with self.assertRaises(ValueError):
                assets.publish(self.args)
            api.assert_not_called()

    def test_partial_upload_never_publishes(self):
        calls = []
        def run(*args, **kwargs):
            if args[:3] == ('gh','api','--paginate'):
                return '[[]]'
            return ''
        def api(path, method='GET', data=None):
            calls.append((path, method, data))
            return {'id':42,'assets':[]}
        with patch.object(assets, 'verify_signature'), patch.object(assets, 'release_state', return_value=({'draft':True, 'id':42}, True)), patch.object(assets, 'run', side_effect=run), patch.object(assets, 'api', side_effect=api):
            with self.assertRaisesRegex(ValueError, 'incomplete'):
                assets.publish(self.args)
        self.assertTrue(all(method != 'PATCH' for _,method,_ in calls))

    def test_same_source_draft_resumes_and_verifies_downloaded_bytes(self):
        calls = []
        def run(*args, **kwargs):
            if args[:3] == ('gh','api','--paginate'):
                return '[[]]'
            if args[:3] == ('gh','release','download'):
                destination = Path(args[args.index('--dir')+1])
                for item in self.directory.iterdir():
                    shutil.copy2(item, destination / item.name)
            return ''
        def api(path, method='GET', data=None):
            calls.append((path,method,data))
            return {'id':42,'assets':[dict(name=p.name,size=p.stat().st_size) for p in self.directory.iterdir()]}
        with patch.object(assets, 'verify_signature'), patch.object(assets, 'release_state', return_value=({'draft':True, 'id':42}, True)), patch.object(assets, 'run', side_effect=run), patch.object(assets, 'api', side_effect=api):
            assets.publish(self.args)
        self.assertEqual(calls[-1], ('releases/42','PATCH',dict(draft=False,prerelease=False,make_latest='true')))
        self.assertFalse(any(method == 'POST' for _,method,_ in calls))


@unittest.skipUnless(shutil.which('tauri') and shutil.which('minisign'), 'real signing tools required')
class RealSignatures(unittest.TestCase):
    def test_sign_validate_reject_tamper_and_wrong_key(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            payloads = root / 'assets'
            payloads.mkdir()
            for name in assets.ASSETS:
                (payloads / name).write_bytes(b'payload bytes')
            key = root / 'test.key'
            assets.run('tauri', 'signer', 'generate', '--ci', '--password', '', '--write-keys', str(key))
            args = type('Args', (), dict(directory=str(payloads), version='0.1.0-rc.14', pubkey=str(key)+'.pub'))()
            with patch.dict(os.environ, TAURI_SIGNING_PRIVATE_KEY=key.read_text().strip(), TAURI_SIGNING_PRIVATE_KEY_PASSWORD=''):
                assets.sign(args)
            result = json.loads((payloads / 'latest.json').read_text())
            self.assertEqual(len(result['platforms']), 4)
            payload = payloads / 'TutaBridge-Linux.AppImage'
            signature = (payloads / (payload.name + '.sig')).read_text()
            pubkey = Path(str(key)+'.pub').read_text()
            assets.verify_signature(payload, signature, pubkey)
            payload.write_bytes(b'tampered')
            with self.assertRaises(subprocess.CalledProcessError):
                assets.verify_signature(payload, signature, pubkey)
            payload.write_bytes(b'payload bytes')
            other = root / 'other.key'
            assets.run('tauri', 'signer', 'generate', '--ci', '--password', '', '--write-keys', str(other))
            with self.assertRaises(subprocess.CalledProcessError):
                assets.verify_signature(payload, signature, Path(str(other)+'.pub').read_text())


class Packaging(unittest.TestCase):
    def test_mac_archive_keeps_app_root_and_executable_mode(self):
        with tempfile.TemporaryDirectory() as root, cwd(root):
            app = Path('target/release/bundle/macos/Test.app/Contents/MacOS')
            app.mkdir(parents=True)
            executable = app / 'test'
            executable.write_bytes(b'binary')
            executable.chmod(0o755)
            Path('target/release/bundle/dmg').mkdir()
            Path('target/release/bundle/dmg/test.dmg').write_bytes(b'dmg')
            Path('target/release/tutabridge').write_bytes(b'cli')
            args = type('Args', (), dict(directory='assets', bundle='target/release/bundle', platform='macOS'))()
            assets.collect(args)
            with tarfile.open('assets/TutaBridge-macOS.app.tar.gz') as archive:
                members = archive.getmembers()
                self.assertTrue(all(m.name.startswith('Test.app') for m in members))
                binary = archive.getmember('Test.app/Contents/MacOS/test')
                self.assertEqual(binary.mode & 0o111, 0o111)


if __name__ == '__main__':
    unittest.main()
