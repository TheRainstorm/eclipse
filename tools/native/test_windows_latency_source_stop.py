import ast,pathlib,tempfile,unittest
source=pathlib.Path(__file__).with_name('windows_latency_worker.py').read_text()
node=next(n for n in ast.parse(source).body if isinstance(n,ast.FunctionDef) and n.name=='source_stop_file')
namespace={'Path':pathlib.Path};exec(compile(ast.Module(body=[node],type_ignores=[]),'worker-helper','exec'),namespace);stop=namespace['source_stop_file']
class SourceStop(unittest.TestCase):
 def test_existing_roles_have_no_control(self):
  self.assertIsNone(stop({'role':'host'},pathlib.Path('/tmp')))
 def test_control_is_only_for_source_parent(self):
  with self.assertRaises(ValueError):stop({'role':'host','parent_stop_file':'/tmp/stop'},pathlib.Path('/tmp'))
 def test_fresh_owned_control(self):
  with tempfile.TemporaryDirectory() as d:
   root=pathlib.Path(d).resolve();self.assertEqual(stop({'role':'stimulus-clock','parent_stop_file':str(root/'stop')},root),root/'stop')
   (root/'stop').touch()
   with self.assertRaises(ValueError):stop({'role':'stimulus-clock','parent_stop_file':str(root/'stop')},root)
 def test_escape_is_rejected(self):
  with tempfile.TemporaryDirectory() as d:
   root=pathlib.Path(d).resolve()
   with self.assertRaises(ValueError):stop({'role':'stimulus-clock','parent_stop_file':str(root/'..'/'outside')},root)
if __name__=='__main__':unittest.main()
