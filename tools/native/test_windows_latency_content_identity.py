import unittest
import windows_latency_scene as scene
class DecodedIdentity(unittest.TestCase):
 def setUp(self):
  self.s=scene.manifest({'fps':60,'stimulus_fps':120,'stimulus_scene':{'mode':'full_source_v1','bounds':[0,0,3840,2160],'seed':734219}});self.phase=7;self.counter=23;self.grid=scene.texture_grid(self.s,self.phase)
 def pixel(self,x,y):
  w,h=self.s['bounds'][2:];points=[(w//2,3),(w-4,h//2),(w//2,h-4),(3,h//2),(16,16),(w-17,16),(16,h-17),(w-17,h-17)];colours=self.s['border_RGB']+self.s['corner_RGB']
  if (x,y)in points:return list(colours[points.index((x,y))])
  if y==96 and 1136<=x<=1472:
   bit=(x-1136)//48;v=224 if ((self.phase>>(bit%4))&1)^(bit//4) else 32;return [v]*3
  if y==302:
   b=(x-112)//160;n=self.counter;return [(n*3+b*37)%220+20,(b*41+n)%220+20,(n*2+b*31)%220+20]
  if 1280<=x<2304 and 768<=y<1792:
   cx=(x-1280)//4;cy=(y-768)//4;at=(cy*256+cx)*3;return list(self.grid[at:at+3])
  return self.s['background']
 def evaluate(self,pixel):
  strict=scene.validate_pixels(self.s,pixel);return strict,scene.decoded_content_identity(self.s,strict)
 def test_design_colours_and_mild_loss_keep_separate_fidelity(self):
  strict,identity=self.evaluate(self.pixel);self.assertTrue(strict['passed']);self.assertTrue(identity['valid']);self.assertEqual(identity['exclusive_radii'],{'borders':[40,30,70,60],'corners':[40,60,70,30]})
  strict,identity=self.evaluate(lambda x,y:[186,32,193]if(x,y)==(16,2143)else self.pixel(x,y));self.assertFalse(strict['passed']);self.assertFalse(identity['strict_fidelity_passed']);self.assertTrue(identity['valid']);self.assertEqual(strict['raw_samples']['corners'][2]['RGB'],[186,32,193])
 def test_black_background_swapped_and_shifted_markers_rejected(self):
  variants=[lambda x,y:[0,0,0],lambda x,y:self.s['background']if(x,y)==(16,16)else self.pixel(x,y),lambda x,y:self.s['corner_RGB'][1]if(x,y)==(16,16)else self.pixel(x,y),lambda x,y:self.pixel(x+32,y),lambda x,y:[0,0,0]if x>400 else self.pixel(x,y)]
  for variant in variants:
   with self.subTest(variant=variant):self.assertFalse(self.evaluate(variant)[1]['valid'])
 def test_exclusive_radius_boundary_and_tie_are_rejected(self):
  for colour,valid,unique in [([240,79,40],True,True),([240,40,80],False,True),([230,80,40],False,False),([240,81,40],False,False)]:
   with self.subTest(colour=colour):
    strict,identity=self.evaluate(lambda x,y:colour if(x,y)==(1920,3)else self.pixel(x,y));self.assertEqual(identity['valid'],valid);self.assertEqual(identity['markers']['borders'][0]['unique_nearest_target'],unique);self.assertFalse(strict['passed'])
 def test_wrong_texture_and_consistent_wrong_barcode_phase_rejected(self):
  def wrong_texture(x,y):return [24,28,36]if 1280<=x<2304 and 768<=y<1792 else self.pixel(x,y)
  self.assertFalse(self.evaluate(wrong_texture)[1]['valid'])
  def wrong_phase(x,y):
   if y==96 and 1136<=x<=1472:
    bit=(x-1136)//48;v=224 if ((6>>(bit%4))&1)^(bit//4)else 32;return [v]*3
   return self.pixel(x,y)
  strict,identity=self.evaluate(wrong_phase);self.assertTrue(strict['checks']['phase_marker']);self.assertFalse(identity['checks']['texture_matches_phase']);self.assertFalse(identity['valid'])
if __name__=='__main__':unittest.main()
