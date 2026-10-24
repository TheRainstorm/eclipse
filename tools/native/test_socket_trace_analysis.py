import unittest
from socket_trace_analysis import analyze

class Matching(unittest.TestCase):
    def report(self, headers):
        return dict(instance='one',wire=[dict(direction=0,outcome=0,header=h,start_us=10,return_us=20,previous_return_us=9) for h in headers],
                    primary=[dict(epoch=1,track=0,header=h,read_us=25,pipeline_us=100) for h in headers],wire_evicted=0,primary_evicted=0)
    def header(self,seq=65535,rtp=4294967295):
        return dict(ssrc=9,rtp=rtp,seq=seq,pt=106,marker=True)
    def test_wrap_is_exact_and_connection_epochs_do_not_merge(self):
        a=self.report([self.header(),self.header(0,3)])
        self.assertEqual(analyze(a)['matched'],2)
        b=self.report([self.header()]);b['instance']='two';b['primary'][0]['epoch']=2
        self.assertEqual(analyze(b)['frames'][0]['epoch'],2)
    def test_duplicates_and_original_ssrc_retransmissions_are_ambiguous(self):
        a=self.report([self.header(),self.header()]);r=analyze(a)
        self.assertEqual((r['matched'],r['ambiguous'],r['duplicate_wire_headers']),(0,2,1))
    def test_synthetic_fec_unknown_ssrc_unmatched_and_negative_are_not_latency(self):
        a=self.report([self.header()]);a['primary'][0]['header']=dict(self.header(),ssrc=10)
        r=analyze(a);self.assertEqual((r['matched'],r['unmatched'],r['unknown_ssrc_packets']),(0,1,1))
        a=self.report([self.header()]);a['primary'][0]['read_us']=19
        self.assertEqual(analyze(a)['negative'],1)
    def test_unique_queue_delay_and_overflow_are_explicit(self):
        a=self.report([self.header()]);a['wire_evicted']=5;r=analyze(a)
        self.assertEqual(r['frames'][0]['first']['queue_us'],5);self.assertEqual(r['wire_evicted'],5)

if __name__=='__main__':unittest.main()
