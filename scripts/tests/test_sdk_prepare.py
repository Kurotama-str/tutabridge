import contextlib
import hashlib
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('prepare_sdk',SCRIPTS/'sdk-prepare.py')
prep=importlib.util.module_from_spec(spec);spec.loader.exec_module(prep)


class Preparation(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name)/'SDK épreuve';self.root.mkdir()
        self.source=self.root/'tuta-repo';self.source.mkdir()
        for folder in (self.root,self.source):
            prep.git(folder,'init','-q')
            prep.git(folder,'config','user.name','Fixture')
            prep.git(folder,'config','user.email','fixture@example.test')
            prep.git(folder,'config','commit.gpgSign','false')
        files={'Cargo.toml':'[workspace]\n','LICENSE.txt':'license\n','tuta-sdk/rust/example.rs':'before\n',
               'test/tests/api/worker/crypto/CompatibilityTestData.json':'{}\n'}
        for name,data in files.items():
            path=self.source/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_text(data)
        prep.git(self.source,'add','.');prep.git(self.source,'commit','-qm','base')
        self.base=prep.git(self.source,'rev-parse','HEAD')
        (self.source/'tuta-sdk/rust/example.rs').write_text('after\n')
        diff=subprocess.check_output(['git','-C',str(self.source),'diff','--binary'])
        prep.git(self.source,'checkout','--','.')
        (self.root/'sdk/patches').mkdir(parents=True)
        (self.root/'sdk/patches/01.patch').write_bytes(diff)
        (self.root/'sdk/BASE').write_text('tag=fixture\ncommit='+self.base+'\n')
        (self.root/'.gitmodules').write_text('[submodule "tuta-repo"]\npath = tuta-repo\nurl = '+prep.UPSTREAM+'\n')
        prep.git(self.root,'add','sdk','.gitmodules')
        prep.git(self.root,'update-index','--add','--cacheinfo',f'160000,{self.base},tuta-repo')
        self.expected=prep.prepare(self.root,update_expected=True)

    def test_idempotent_offline_and_source_clean(self):
        before={p:p.stat().st_mtime_ns for p in (self.root/'sdk/generated').rglob('*')}
        with patch.object(prep,'generate',side_effect=AssertionError('Should not regenerate')):
            self.assertEqual(prep.prepare(self.root),self.expected)
        self.assertEqual(before,{p:p.stat().st_mtime_ns for p in (self.root/'sdk/generated').rglob('*')})
        self.assertEqual(prep.git(self.source,'status','--porcelain'),'')
        self.assertEqual(prep.git(self.source,'rev-parse','HEAD'),self.base)

    def test_output_corruption_missing_extra_and_symlink(self):
        folder=self.root/'sdk/generated';p=folder/'tuta-sdk/rust/example.rs'
        for change in ('edit','delete','extra','symlink'):
            with self.subTest(change=change):
                if change=='edit':p.write_text('bad')
                elif change=='delete':p.unlink()
                elif change=='extra':(folder/'extra.txt').write_text('extra')
                else:p.unlink();p.symlink_to(self.source/'LICENSE.txt')
                with self.assertRaises(ValueError):prep.check(self.root)
                prep.prepare(self.root)
                self.assertEqual(p.read_text(),'after\n')

    def test_changed_inputs_fail_without_replacing_previous_output(self):
        path=self.root/'sdk/patches/01.patch';original=path.read_bytes()
        for value in (b'bad',None):
            if value is None:path.unlink()
            else:path.write_bytes(value)
            with self.assertRaises(ValueError):prep.prepare(self.root)
            path.write_bytes(original)
        base=self.root/'sdk/BASE';original_base=base.read_text()
        base.write_text(original_base+'commit='+self.base+'\n')
        with self.assertRaisesRegex(ValueError,'Invalid sdk/BASE commit'):prep.prepare(self.root)
        base.write_text('commit='+'a'*40+'\n')
        with self.assertRaises(ValueError):prep.prepare(self.root)
        self.assertEqual(prep.content_digest(self.root/'sdk/generated'),self.expected['content'])

    def test_archive_ignores_windows_autocrlf_and_injected_git_config(self):
        prep.git(self.source,'config','core.autocrlf','true')
        with patch.dict(os.environ,GIT_CONFIG_PARAMETERS="'core.autocrlf=true'"):
            shutil.rmtree(self.root/'sdk/generated')
            self.assertEqual(prep.prepare(self.root),self.expected)
        self.assertNotIn(b'\r', (self.root/'sdk/generated/Cargo.toml').read_bytes())

    def test_crlf_checkout_uses_its_config_but_exports_lf(self):
        config=self.root/'checkout.gitconfig'
        config.write_text('[core]\n    autocrlf = true\n')
        files=prep.git(self.source,'ls-files').splitlines()
        for scope in ('local','global','system'):
            with self.subTest(scope=scope),patch.dict(os.environ):
                os.environ.pop('GIT_CONFIG_NOSYSTEM',None)
                os.environ['GIT_CONFIG_SYSTEM']=str(config) if scope=='system' else os.devnull
                os.environ['GIT_CONFIG_GLOBAL']=str(config) if scope=='global' else os.devnull
                if scope=='local':prep.git(self.source,'config','core.autocrlf','true')
                # Actually materialize CRLF, then invalidate Git's cached stat.
                for name in files:(self.source/name).unlink()
                subprocess.run(['git','-C',str(self.source),'checkout-index','--all','--index'],check=True)
                for name in files:
                    p=self.source/name
                    self.assertIn(b'\r\n',p.read_bytes())
                    st=p.stat();os.utime(p,ns=(st.st_atime_ns,st.st_mtime_ns+2_000_000_000))
                self.assertTrue(prep.git(self.source,'status','--porcelain','--ignore-submodules=all'))
                shutil.rmtree(self.root/'sdk/generated')
                self.assertEqual(prep.prepare(self.root),self.expected)
                for p in (self.root/'sdk/generated').rglob('*'):
                    if p.is_file():self.assertNotIn(b'\r',p.read_bytes())
                edited=self.source/'LICENSE.txt';edited.write_bytes(b'changed\r\n')
                with self.assertRaisesRegex(ValueError,r'local changes:[\s\S]*LICENSE.txt'):
                    prep.check(self.root)
                subprocess.run(['git','-C',str(self.source),'checkout','--','LICENSE.txt'],check=True)
                if scope=='local':prep.git(self.source,'config','--unset','core.autocrlf')

    def test_nested_patches_rejected_and_already_applied_diagnostic(self):
        nested=self.root/'sdk/patches/nested';nested.mkdir()
        with self.assertRaisesRegex(ValueError,'one directory'):prep.inputs(self.root)
        nested.rmdir()
        shutil.copy2(self.root/'sdk/patches/01.patch',self.root/'sdk/patches/02.patch')
        with self.assertRaisesRegex(ValueError,'already included'):prep.prepare(self.root,update_expected=True)

    def test_tree_mismatch_and_dirty_official_checkout_refused(self):
        good=(self.root/'sdk/EXPECTED').read_text()
        (self.root/'sdk/EXPECTED').write_text(good.replace(self.expected['tree'],'0'*40))
        # Force regeneration rather than relying on an already verified output.
        shutil.rmtree(self.root/'sdk/generated')
        with self.assertRaisesRegex(ValueError,'tree'):prep.prepare(self.root)
        (self.root/'sdk/EXPECTED').write_text(good)
        (self.source/'LICENSE.txt').write_text('changed')
        with self.assertRaisesRegex(ValueError,'local changes'):prep.prepare(self.root)

    def test_archive_path_and_symlink_refused(self):
        import io,tarfile
        for name,kind in [('../escape',tarfile.REGTYPE),('link',tarfile.SYMTYPE)]:
            stream=io.BytesIO()
            with tarfile.open(fileobj=stream,mode='w') as tar:
                item=tarfile.TarInfo(name);item.type=kind;tar.addfile(item)
            with patch.object(subprocess,'check_output',return_value=stream.getvalue()):
                with self.assertRaises(ValueError):prep.extract(self.source,'a'*40,self.root/'out')
        self.assertFalse((self.root/'escape').exists())
        # Failure in staged extraction must leave the old SDK intact.
        real_extract=prep.extract
        def fail_extract(source,tree,output):
            real_extract(source,tree,output)
            raise OSError('extraction failed')
        with patch.object(prep,'extract',side_effect=fail_extract):
            with self.assertRaisesRegex(OSError,'extraction failed'):
                prep.prepare(self.root,update_expected=True)
        self.assertEqual(prep.check(self.root),self.expected)

    def test_compile_paths_never_consume_official_unpatched_files(self):
        repo=SCRIPTS.parent
        paths=[repo/'Cargo.toml']
        for directory in ['crates','src-tauri']:
            paths.extend(p for p in (repo/directory).rglob('*') if p.suffix in ('.rs','.toml'))
        for path in paths:
            self.assertNotIn('tuta-repo/',path.read_text(),str(path))

    @unittest.skipUnless(shutil.which('cargo'),'Cargo required for real Rust guard test')
    def test_actual_rust_build_guard_rejects_stale_and_corrupt_sdk(self):
        crate=self.root/'crates/guard';(crate/'src').mkdir(parents=True)
        (crate/'src/lib.rs').write_text('pub fn fixture() {}\n')
        (crate/'Cargo.toml').write_text('[package]\nname="sdk-guard-fixture"\nversion="0.1.0"\nedition="2021"\n[workspace]\n[build-dependencies]\nsha2="0.10"\n')
        shutil.copy2(SCRIPTS.parent/'crates/tuta/build.rs',crate/'build.rs')
        def build():
            return subprocess.run(['cargo','check','--offline','--manifest-path',str(crate/'Cargo.toml')],
                capture_output=True,text=True,env=dict(os.environ,CARGO_TARGET_DIR=str(self.root/'guard-target')))
        result=build();self.assertEqual(result.returncode,0,result.stderr)
        patches=self.root/'sdk/patches'
        (patches/'01.patch').rename(patches/'01-B.patch')
        (self.source/'LICENSE.txt').write_text('new license\n')
        (patches/'01-a.patch').write_bytes(subprocess.check_output(['git','-C',str(self.source),'diff','--binary']))
        prep.git(self.source,'checkout','--','.')
        (patches/'unused.PATCH').write_text('ignored, exact suffix only')
        self.expected=prep.prepare(self.root,update_expected=True)
        self.assertEqual([n for n,_ in self.expected['patches']],['01-B.patch','01-a.patch'])
        result=build();self.assertEqual(result.returncode,0,result.stderr)
        folder=self.root/'sdk/generated';p=folder/'tuta-sdk/rust/example.rs'
        for change in ('edit','missing','extra','symlink','patch','base','duplicate-base','expected-order','absent'):
            with self.subTest(change=change):
                originals={q:q.read_bytes() for q in (self.root/'sdk').rglob('*') if q.is_file()}
                if change=='edit':p.write_text('bad')
                elif change=='missing':p.unlink()
                elif change=='extra':(folder/'extra').write_text('bad')
                elif change=='symlink':p.unlink();p.symlink_to(self.source/'LICENSE.txt')
                elif change=='patch':(patches/'01-B.patch').write_text('bad')
                elif change=='base':(self.root/'sdk/BASE').write_text('commit='+'0'*40+'\n')
                elif change=='duplicate-base':
                    b=self.root/'sdk/BASE';b.write_text(b.read_text()+'commit='+self.base+'\n')
                    with self.assertRaisesRegex(ValueError,'Invalid sdk/BASE commit'):prep.inputs(self.root)
                elif change=='expected-order':
                    e=self.root/'sdk/EXPECTED';e.write_text(e.read_text()+'patch=extra:0\n')
                else:shutil.rmtree(folder)
                result=build();self.assertNotEqual(result.returncode,0,result.stderr)
                self.assertIn('scripts/sdk-prepare.py',result.stderr)
                if folder.exists():shutil.rmtree(folder)
                for q,data in originals.items():q.parent.mkdir(parents=True,exist_ok=True);q.write_bytes(data)
                result=build();self.assertEqual(result.returncode,0,result.stderr)
