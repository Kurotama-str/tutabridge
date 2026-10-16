"""Real offline SDK pipeline; only GitHub API responses are fixtures."""
import base64
import contextlib
import gzip
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('pipeline_release', SCRIPTS/'sdk-release.py')
sdk = importlib.util.module_from_spec(spec); spec.loader.exec_module(sdk)
prep = sdk.preparation()


class OfflinePipeline(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='pipeline-'); self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)/'SDK épreuve'; self.root.mkdir()
        self.up = self.root/'up'; self.up.mkdir()
        self.remote = self.root/'bridge.git'; self.up_remote = self.root/'up.git'
        self.bridge = self.root/'bridge'; self.bridge.mkdir()
        self.writer_bin = self.root/'writer-bin'; self.writer_bin.mkdir()
        self.api_bin = self.root/'api-bin'; self.api_bin.mkdir()
        for name, executable in [('git',shutil.which('git')),('python3',sys.executable)]:
            (self.writer_bin/name).symlink_to(executable)
        self.release_file = self.root/'releases.json'; self.release_file.write_text('[]')
        gh = self.api_bin/'gh'
        gh.write_text(f'#!{sys.executable}\nimport json,os,sys\nassert sys.argv[1:]==["api","--paginate","--slurp","repos/{sdk.REPO}/releases?per_page=100"]\nprint(json.dumps([json.load(open(os.environ["FIXTURE_RELEASES"]))]))\n')
        gh.chmod(0o755)
        config = self.root/'gitconfig'; config.write_text('')
        for remote,url in [(self.up_remote,sdk.UPSTREAM),(self.remote,'https://github.com/'+sdk.REPO+'.git')]:
            sdk.git('config','--file',str(config),f'url.{remote}.insteadOf',url)
        env=dict(GIT_CONFIG_GLOBAL=str(config),GIT_CONFIG_NOSYSTEM='1',GIT_ALLOW_PROTOCOL='file',
                 CARGO_NET_OFFLINE='true',CARGO_TARGET_DIR=str(self.root/'target'),
                 GH_TOKEN='local-fixture',GITHUB_RUN_ID='0',FIXTURE_RELEASES=str(self.release_file),
                 PATH=str(self.api_bin)+os.pathsep+os.environ['PATH'],GIT_TERMINAL_PROMPT='0',
                 GIT_AUTHOR_NAME=sdk.BOT_NAME,GIT_AUTHOR_EMAIL=sdk.BOT_EMAIL,
                 GIT_COMMITTER_NAME=sdk.BOT_NAME,GIT_COMMITTER_EMAIL=sdk.BOT_EMAIL)
        self.checkout_path=env['PATH']
        self.enterContext(patch.dict(os.environ,env))
        self.enterContext(patch('urllib.request.urlopen',side_effect=AssertionError('Network forbidden')))
        self.pulls=[]; self.enterContext(patch.object(sdk,'api',side_effect=self.api))
        for folder in [self.up,self.bridge]:
            sdk.git('-C',str(folder),'init','-q','-b','main')
            sdk.git('-C',str(folder),'config','commit.gpgSign','false')
        with contextlib.chdir(self.up):
            self.write('Cargo.toml','[workspace]\nmembers=["tuta-sdk/rust/sdk"]\nresolver="2"\n[workspace.package]\nversion="359.0.0"\n')
            self.write('tuta-sdk/rust/sdk/Cargo.toml','[package]\nname="tuta-sdk"\nversion.workspace=true\nedition="2021"\n')
            library='pub fn answer() -> u8 {\n    1\n}\n\n#[test]\nfn answer_is_available() {\n    assert_eq!(answer(), 1);\n}\n'
            self.write('tuta-sdk/rust/sdk/src/lib.rs',library)
            self.write('LICENSE.txt','fixture\n')
            self.write('test/tests/api/worker/crypto/CompatibilityTestData.json','{}\n')
            old=self.commit('upstream359')
            self.write('tuta-sdk/rust/sdk/src/lib.rs',library.replace('    1\n','    2\n').replace('answer(), 1','answer(), 2'))
            patch_bytes=subprocess.check_output(['git','diff','--binary'])
            sdk.git('checkout','--','.')
            p=Path('Cargo.toml');p.write_text(p.read_text().replace('359.0.0','360.0.0'))
            current=self.commit('upstream360');sdk.git('tag','tutanota-release-360.260922.0')
            sdk.git('clone','--bare','-q','.',str(self.up_remote))
        with contextlib.chdir(self.bridge):
            self.write('.gitignore','/sdk/generated/\n/target/\n')
            self.write('.gitmodules','[submodule "tuta-repo"]\npath = tuta-repo\nurl = https://github.com/spartanz51/tutanota.git\nbranch = generated/359.260901.0-'+old[:12]+'\n')
            self.write('sdk/BASE','tag=tutanota-release-359.260901.0\ncommit='+old+'\n')
            self.write('sdk/patches/01.patch',patch_bytes)
            self.write('Cargo.toml','[workspace]\nexclude=["tuta-repo","sdk/generated"]\n[package]\nname="bridge"\nversion="1.0.0"\nedition="2021"\n[dependencies]\ntuta-sdk={path="sdk/generated/tuta-sdk/rust/sdk"}\n')
            self.write('src/lib.rs','pub fn answer() -> u8 { tuta_sdk::answer() }\n')
            self.write('Cargo.lock','version = 4\n\n[[package]]\nname = "bridge"\nversion = "1.0.0"\ndependencies = ["tuta-sdk"]\n\n[[package]]\nname = "tuta-sdk"\nversion = "359.0.0"\n')
            for name in ['sdk-prepare.py','sdk-release.py']:
                self.write('scripts/'+name,(SCRIPTS/name).read_bytes())
            sdk.git('add','.');sdk.git('update-index','--add','--cacheinfo',f'160000,{old},tuta-repo')
            parent=self.commit('legacy base',old)
            sdk.set_base('tutanota-release-360.260922.0',current)
            sdk.git('config','-f','.gitmodules','submodule.tuta-repo.branch','generated/360.260922.0-'+current[:12])
            p=Path('Cargo.lock');p.write_text(p.read_text().replace('359.0.0','360.0.0'))
            plan=dict(version='0.1.0-rc.13',tag='tutanota-release-360.260922.0',upstream=current,generated=current,base=parent,lock=hashlib.sha256(p.read_bytes()).hexdigest())
            self.write('release-plan.json',json.dumps(plan))
            sdk.git('update-index','--add','--cacheinfo',f'160000,{current},tuta-repo')
            legacy=self.commit('legacy candidate',current);sdk.validate_candidate(legacy)
            sdk.git('tag','v0.1.0-rc.13');self.publish(plan['version'])
            self.write('.gitmodules','[submodule "tuta-repo"]\npath = tuta-repo\nurl = '+sdk.UPSTREAM+'\n')
            sdk.git('submodule','update','--init','--quiet','tuta-repo')
            prep.prepare(Path.cwd(),update_expected=True)
            sdk.run('cargo','generate-lockfile','--offline')
            self.base=self.commit('official migration')
            sdk.git('clone','--bare','-q','.',str(self.remote))
            sdk.git('remote','add','origin','https://github.com/'+sdk.REPO+'.git')
        self.counter=0

    @staticmethod
    def write(name,data):
        p=Path(name);p.parent.mkdir(parents=True,exist_ok=True)
        p.write_bytes(data if isinstance(data,bytes) else data.encode())

    @staticmethod
    def commit(message,gitlink=None):
        sdk.git('add','.')
        if gitlink:sdk.git('update-index','--add','--cacheinfo',f'160000,{gitlink},tuta-repo')
        sdk.git('commit','-qm',message)
        return sdk.git('rev-parse','HEAD')

    def api(self,path,method='GET',data=None):
        if path.startswith('pulls?') and method=='GET':
            return [p for p in self.pulls if path.endswith(':'+p['head'])]
        self.assertEqual((path,method),('pulls','POST'))
        self.pulls.append(data);return data

    def publish(self,version):
        rows=json.loads(self.release_file.read_text())
        rows.append(dict(tag_name='v'+version,draft=False))
        self.release_file.write_text(json.dumps(rows))

    def stage(self,fn):
        self.counter+=1;out=self.root/f'outputs-{self.counter}'
        with patch.dict(os.environ,GITHUB_OUTPUT=str(out)):
            fn()
        return dict(line.split('=',1) for line in out.read_text().splitlines()) if out.exists() else {}

    def detect(self):
        return self.stage(lambda:sdk.detect(SimpleNamespace(scheduled=False,dry_run=False,tag='')))

    def bump(self,version,broken=False):
        with contextlib.chdir(self.up):
            p=Path('Cargo.toml');text=p.read_text();text=text.replace('360.0.0',version).replace('361.0.0',version);p.write_text(text)
            self.counter+=1;self.write('LICENSE.txt',f'fixture revision {self.counter}\n')
            if broken:self.write('tuta-sdk/rust/sdk/src/lib.rs','pub fn upstream_changed() {}\n')
            sha=self.commit('next upstream')
            number=361+len(sdk.git('tag','--list').splitlines())-1
            tag=f'tutanota-release-{number}.261015.0'
            sdk.git('tag',tag);sdk.git('push','--quiet',str(self.up_remote),'main','--tags')
        return sha

    def clone(self,base):
        self.counter+=1;folder=self.root/f'job-{self.counter}'
        # Checkout is a separate runner step; only writers have restricted PATH.
        with patch.dict(os.environ,PATH=self.checkout_path):
            sdk.git('clone','--quiet','https://github.com/'+sdk.REPO+'.git',str(folder))
            sdk.git('-C',str(folder),'checkout','--quiet','--detach',base)
            sdk.git('-C',str(folder),'submodule','update','--init','--quiet','tuta-repo')
        return folder

    def verify(self,selection,success=True):
        folder=self.clone(selection['base'])
        with contextlib.chdir(folder):
            sdk.set_base(selection['tag'],selection['upstream'])
            sdk.git('-C','tuta-repo','fetch','--quiet',sdk.UPSTREAM,selection['upstream'])
            sdk.git('-C','tuta-repo','checkout','--quiet','--detach',selection['upstream']);sdk.git('add','tuta-repo')
            workflow=(SCRIPTS.parent/'.github/workflows/sdk-bump.yml').read_text()
            body=workflow.split('        id: generate\n',1)[1].split('        run: |\n',1)[1]
            lines=[]
            for line in body.splitlines():
                if line and not line.startswith('          '):break
                lines.append(line[10:])
            def generate():
                with patch.dict(os.environ,TAG=selection['tag'],GITHUB_STEP_SUMMARY=str(self.root/'summary')):
                    result=subprocess.run(['bash','-eo','pipefail','-c','\n'.join(lines)])
                    self.assertEqual(result.returncode==0,success)
            result=self.stage(generate)
            if success:result.update(self.stage(lambda:sdk.lock(None)))
            else:self.assertEqual(result['generated'],'')
            return result

    def writer(self,selection,proof,success=True):
        folder=self.clone(selection['base'])
        env=dict(BASE=selection['base'],TAG=selection['tag'],UPSTREAM_SHA=selection['upstream'],
                 VERSION=selection['version'],VERIFIED='success' if success else 'failure',
                 GENERATED=proof['generated'],LOCK_DIFF=proof.get('lock_diff',''),LOCK_HASH=proof.get('lock_hash',''),
                 PATH=str(self.writer_bin))
        return folder,env

    def refs(self,remote):
        return sdk.git('--git-dir',str(remote),'show-ref')

    def test_real_legacy_migration_bumps_resume_and_unchanged_lock(self):
        for version,changed in [('361.0.0',True),('361.0.0',False)]:
            with self.subTest(version=version,changed=changed),contextlib.chdir(self.bridge):
                self.bump(version);selection=self.detect();self.assertEqual(selection['mode'],'bump')
                upstream_before=self.refs(self.up_remote)
                proof=self.verify(selection)
                self.assertEqual(bool(gzip.decompress(base64.b64decode(proof['lock_diff']))),changed)
                folder,env=self.writer(selection,proof)
                before=self.refs(self.remote)
                with contextlib.chdir(folder),patch.dict(os.environ,env):
                    self.assertIsNone(shutil.which('cargo'));self.assertIsNone(shutil.which('gh'))
                    result=self.stage(lambda:sdk.candidate(None));self.assertEqual(result['ready'],'true')
                    source=result['source'];plan=sdk.validate_candidate(source,selection['base'],proof['generated'])
                    expected=prep.check(folder)
                    self.assertEqual(expected['tree'],plan['tree']);self.assertEqual(expected['base'],plan['upstream'])
                    self.assertEqual(expected['content'],prep.content_digest(folder/'sdk/generated'))
                    self.assertEqual(hashlib.sha256(Path('Cargo.lock').read_bytes()).hexdigest(),plan['lock'])
                    actual=set(sdk.git('diff-tree','--no-commit-id','--name-only','-r',source).splitlines())
                    self.assertEqual(actual,sdk.ALLOWED if changed else sdk.ALLOWED-{'Cargo.lock'})
                    self.assertEqual(self.refs(self.up_remote),upstream_before)
                    new=set(self.refs(self.remote).splitlines())-set(before.splitlines())
                    self.assertEqual(len(new),1);self.assertIn('refs/heads/sdk/',next(iter(new)))
                    # A workflow retry reuses the reviewed source and existing PR.
                    branch=next(iter(new)).split()[1].removeprefix('refs/heads/')
                    refs_before_retry=self.refs(self.remote);pulls_before=list(self.pulls)
                    retry,retry_env=self.writer(selection,proof)
                    with contextlib.chdir(retry),patch.dict(os.environ,retry_env):
                        repeated=self.stage(lambda:sdk.candidate(None))
                    self.assertEqual(repeated,result)
                    self.assertEqual(self.refs(self.remote),refs_before_retry)
                    self.assertEqual(self.pulls,pulls_before)
                    # Edits to that branch must never be overwritten on retry.
                    self.write('review-note.txt','human branch edit')
                    edited=self.commit('edit candidate branch')
                    sdk.push('https://github.com/'+sdk.REPO+'.git',edited,branch)
                    refs_after_edit=self.refs(self.remote)
                    retry,retry_env=self.writer(selection,proof)
                    with contextlib.chdir(retry),patch.dict(os.environ,retry_env):
                        with self.assertRaisesRegex(ValueError,'Existing branch differs'):
                            sdk.candidate(None)
                    self.assertEqual(self.refs(self.remote),refs_after_edit)
                    self.assertEqual(self.pulls,pulls_before)
                    with patch.dict(os.environ,SOURCE=source):sdk.integrate(None)
                    self.assertEqual(sdk.git('ls-remote','origin','refs/heads/main').split()[0],source)
                sdk.git('fetch','--quiet','origin');sdk.git('merge','--ff-only','origin/main')
                self.assertEqual(self.detect()['mode'],'resume')
                sdk.git('tag','v'+plan['version'],source);self.publish(plan['version'])
                self.assertEqual(self.detect()['mode'],'skip')
                self.assertEqual(self.refs(self.up_remote),upstream_before)

    def test_main_advance_and_tampered_proofs_never_integrate(self):
        with contextlib.chdir(self.bridge):
            self.bump('361.0.0');selection=self.detect();proof=self.verify(selection)
            before=self.refs(self.remote);up_before=self.refs(self.up_remote)
            for key,value,error in [('generated','0'*40,'Regeneration differs'),('lock_hash','0'*64,'differs from the verifier')]:
                with self.subTest(key=key):
                    folder,env=self.writer(selection,dict(proof,**{key:value}))
                    with contextlib.chdir(folder),patch.dict(os.environ,env):
                        with self.assertRaisesRegex(ValueError,error):sdk.candidate(None)
                    self.assertEqual(self.refs(self.remote),before)
            # A non-official base is rejected by the real source verifier.
            folder,env=self.writer(selection,proof)
            with contextlib.chdir(folder):
                sdk.git('config','-f','.gitmodules','submodule.tuta-repo.url','https://example.test/unofficial.git')
                bad=self.commit('unofficial URL')
                with patch.dict(os.environ,dict(env,BASE=bad)):
                    with self.assertRaisesRegex(ValueError,'official Tuta'):sdk.candidate(None)
            self.assertEqual(self.refs(self.remote),before)
            folder,env=self.writer(selection,proof)
            with contextlib.chdir(folder),patch.dict(os.environ,env):
                result=self.stage(lambda:sdk.candidate(None))
            self.write('notes.txt','main advanced');advanced=self.commit('advance main')
            sdk.git('push','--quiet','origin','main')
            with contextlib.chdir(folder),patch.dict(os.environ,dict(env,SOURCE=result['source'])):
                with self.assertRaisesRegex(ValueError,'Main advanced'):sdk.integrate(None)
            self.assertEqual(sdk.git('ls-remote','origin','refs/heads/main').split()[0],advanced)
            self.assertEqual(self.refs(self.up_remote),up_before)

    def test_failed_patch_only_pushes_diagnostic(self):
        with contextlib.chdir(self.bridge):
            self.bump('361.0.0',broken=True);selection=self.detect();proof=self.verify(selection,success=False)
            up_before=self.refs(self.up_remote)
            folder,env=self.writer(selection,proof,success=False)
            with contextlib.chdir(folder),patch.dict(os.environ,env):
                result=self.stage(lambda:sdk.candidate(None))
                self.assertEqual(result['ready'],'false');self.assertEqual(result['source'],'')
                self.assertEqual(sdk.git('diff-tree','--no-commit-id','--name-only','-r','HEAD'),'sdk/BASE')
            self.assertTrue(self.pulls[-1]['head'].endswith('-diagnostic'))
            self.assertEqual(sdk.git('ls-remote','origin','refs/heads/main').split()[0],selection['base'])
            self.assertEqual(self.refs(self.up_remote),up_before)
