import subprocess
import tempfile
import unittest
from pathlib import Path
import source


class SubmoduleProvenance(unittest.TestCase):
    def git(self, root, *args):
        return subprocess.check_output(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', *args], cwd=root, stderr=subprocess.DEVNULL, text=True).strip()

    def test_clean_pinned_plugins_and_missing_dirty_or_replaced_checkout(self):
        with tempfile.TemporaryDirectory(prefix='eclipse-source-fixture-') as temporary:
            base=Path(temporary);main=base/'main';plugin=base/'plugin'
            main.mkdir();plugin.mkdir()
            for root in [main,plugin]:
                self.git(root,'init','-b','main');(root/'README').write_text('generated fixture\n')
                self.git(root,'add','.');self.git(root,'commit','-m','fixture')
            self.git(main,'-c','protocol.file.allow=always','submodule','add',str(plugin),'plugins')
            self.git(main,'commit','-am','pin plugin')
            revision=self.git(main,'rev-parse','HEAD')
            self.assertEqual(source.revision(main,['plugins','.gitmodules']),revision)
            (main/'plugins/untracked').write_text('uncommitted plugin source')
            with self.assertRaises(RuntimeError):source.revision(main,['plugins','.gitmodules'])
            (main/'plugins/untracked').unlink()
            self.git(main/'plugins','commit','--allow-empty','-m','different pin')
            self.git(main,'config','submodule.plugins.ignore','all')
            with self.assertRaises(RuntimeError):source.revision(main,['plugins','.gitmodules'])
            self.git(main,'submodule','update','--init')
            self.git(main,'submodule','deinit','-f','plugins')
            with self.assertRaises(RuntimeError):source.revision(main,['plugins','.gitmodules'])


if __name__=='__main__':unittest.main()
