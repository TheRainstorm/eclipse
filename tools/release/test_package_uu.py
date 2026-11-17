"""Pinned source and shared-SDK packaging checks using owned local fixtures."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import package_uu


class UuPackageTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(); self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def test_exact_seed_commit_is_checked_out_without_changing_seed(self):
        seed = self.root/'seed'; seed.mkdir()
        package_uu.git(seed,'init','-q')
        (seed/'owned.rs').write_text('owned fixture')
        package_uu.git(seed,'add','owned.rs')
        package_uu.git(seed,'-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','fixture')
        revision = package_uu.git(seed,'rev-parse','HEAD')
        source = dict(repository='https://github.com/TheRainstorm/uu-client-plugins.git',revision=revision)
        cached = package_uu.checkout(source,self.root/'cache',seed)
        self.assertEqual(package_uu.git(cached,'rev-parse','HEAD'),revision)
        self.assertEqual(package_uu.git(seed,'status','--porcelain'),'')
        self.assertEqual(package_uu.checkout(source,self.root/'cache',seed),cached)
        (cached/'owned.rs').write_text('changed')
        with self.assertRaisesRegex(RuntimeError,'differs'):package_uu.checkout(source,self.root/'cache',seed)

    def test_unavailable_pin_fails_without_falling_back_to_branch_head(self):
        with patch.object(package_uu.subprocess,'run',side_effect=subprocess.CalledProcessError(1,['git','fetch'])),patch.object(package_uu,'git'):
            with self.assertRaisesRegex(RuntimeError,'Pinned UU commit .* unavailable'):
                package_uu.checkout(dict(repository='https://github.com/TheRainstorm/uu-client-plugins.git',revision='a'*40),self.root/'cache')
        self.assertFalse((self.root/'cache'/('a'*40)).exists())

    def test_workspace_reuses_main_cargo_output_for_config_and_environment(self):
        engine = self.root/'engine'; (engine/'member/src').mkdir(parents=True)
        (engine/'Cargo.toml').write_text('[workspace]\nresolver="2"\nmembers=["member"]\n')
        (engine/'member/Cargo.toml').write_text('[package]\nname="owned-cache-fixture"\nversion="0.1.0"\nedition="2021"\n')
        (engine/'member/src/lib.rs').write_text('pub fn owned() {}\n')
        (engine/'.cargo').mkdir(); (engine/'.cargo/config.toml').write_text('[build]\ntarget-dir="configured-target"\n')
        subprocess.run(['cargo','generate-lockfile','--offline'],cwd=engine,check=True,capture_output=True)
        source = self.root/'source'; (source/'tools').mkdir(parents=True)
        (source/'tools/sdk_workspace.py').write_text(
            'import os,shutil\n'
            'def prepare(engine,source,target):\n'
            '    workspace=source/"mirror";workspace.mkdir(exist_ok=True)\n'
            '    for name in ("Cargo.toml","Cargo.lock"):shutil.copy2(engine/name,workspace/name)\n'
            '    env=os.environ.copy();env["CARGO_TARGET_DIR"]=str(source/"cold-target")\n'
            '    env["CARGO_ENCODED_RUSTFLAGS"]="-C\\x1fprefer-dynamic"\n'
            '    return workspace,env\n')
        for override in (None,self.root/'custom-output'):
            with self.subTest(override=override),patch.dict(os.environ):
                os.environ.pop('CARGO_TARGET_DIR',None)
                if override:os.environ['CARGO_TARGET_DIR']=str(override)
                workspace,environment=package_uu.prepare_workspace(engine,source,'x86_64-unknown-linux-gnu')
            self.assertEqual(Path(environment['CARGO_TARGET_DIR']),override or engine/'configured-target')
            self.assertEqual(environment['CARGO_ENCODED_RUSTFLAGS'],'-C\x1fprefer-dynamic')
            self.assertTrue((workspace/'member/src/lib.rs').is_file())

    def build_fixture(self, sdk='f'*64):
        application = self.root/'application'; application.mkdir()
        runtime = ['uu_plugin_sdk.dll','std-0123456789abcdef.dll']
        for name in runtime:(application/name).write_bytes(b'owned runtime fixture')
        (application/'version.json').write_text(json.dumps(dict(source_commit='a'*40,target='x86_64-pc-windows-gnu',plugin_sdk_id='f'*64,plugin_runtime=runtime)))
        plugin = self.root/'plugin'; plugin.mkdir()
        for name in package_uu.NOTICES:(plugin/name).write_text('owned '+name)
        (self.root/'THIRD-PARTY-NOTICES.txt').write_text('owned engine notice')
        workspace = self.root/'workspace'; workspace.mkdir()
        (workspace/'Cargo.toml').write_text('[workspace.package]\nversion="0.24.7"\n')
        artifacts = self.root/'target'; binary = artifacts/'x86_64-pc-windows-gnu/release/uu_compat.dll'
        binary.parent.mkdir(parents=True); binary.write_bytes(b'owned plugin fixture')
        source = dict(repository='https://github.com/TheRainstorm/uu-client-plugins.git',revision='b'*40)
        patches = [patch.object(package_uu,'ROOT',self.root),patch.object(package_uu,'pin',return_value=source),
            patch.object(package_uu,'checkout',return_value=plugin),
            patch.object(package_uu,'prepare_workspace',return_value=(workspace,{'CARGO_TARGET_DIR':str(artifacts)})),
            patch.object(package_uu.plugin_sdk,'verify_consumer'),
            patch.object(package_uu.plugin_sdk,'build',return_value=(None,None,{'sdk_id':sdk})),
            patch.object(package_uu.plugin_sdk,'strip_windows_binary'),
            patch.object(package_uu.package_plugin,'probe'),
            patch.object(package_uu,'git',side_effect=lambda root,*args:source['revision'] if args[0]=='rev-parse' else '')]
        mocks = [p.start() for p in patches]
        for p in patches:self.addCleanup(p.stop)
        return application,source,mocks

    def test_current_application_sdk_is_probed_and_source_identities_are_separate(self):
        application,source,mocks = self.build_fixture()
        archive = package_uu.build(application,self.root/'build-sdk.dll','x86_64-pc-windows-gnu',[],self.root/'output',self.root/'cache')
        self.assertTrue(mocks[5].call_args.kwargs['library_only'])
        self.assertEqual(mocks[7].call_args.args[1],application/'uu_plugin_sdk.dll')
        self.assertEqual(mocks[7].call_args.args[-1],self.root/'build-sdk.dll')
        with zipfile.ZipFile(archive) as bundle:
            build = json.loads(bundle.read('plugins/uu/build.json'))
            self.assertEqual(build['source_commit'],source['revision'])
            self.assertEqual(build['application_source_commit'],'a'*40)
            plugin = json.loads(bundle.read('plugins/uu/plugin.json'))
            self.assertEqual(plugin['library_sha256'],hashlib.sha256(bundle.read('plugins/uu/uu_compat.dll')).hexdigest())
            self.assertIn('plugins/uu/ENGINE-THIRD-PARTY-NOTICES.txt',bundle.namelist())
            self.assertFalse(any(name.endswith('/state.json') for name in bundle.namelist()))

    def test_sdk_mismatch_stops_before_probe_or_publication(self):
        application,_,mocks = self.build_fixture(sdk='e'*64)
        with self.assertRaisesRegex(RuntimeError,'refusing a fallback'):
            package_uu.build(application,self.root/'build-sdk.dll','x86_64-pc-windows-gnu',[],self.root/'output',self.root/'cache')
        mocks[7].assert_not_called()
        self.assertFalse((self.root/'output').exists())


if __name__ == '__main__':
    unittest.main()
