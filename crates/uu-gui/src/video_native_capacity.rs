//! Bounded decoder leases and progress-aware admission. No GPU waits or I/O.
use std::{collections::VecDeque,time::{Duration,Instant}};
use serde::Serialize;

const FIRST_POLL:Duration=Duration::from_millis(2);
const MAX_POLL:Duration=Duration::from_millis(16);

pub(super) fn request_retry(ctx:&eframe::egui::Context,viewport:eframe::egui::ViewportId,retry:Duration){
    // egui subtracts predicted_dt before waking its owner. Preserve the real
    // retry deadline instead of turning a capacity poll into a busy UI loop.
    let predicted=ctx.input_for(viewport,|i|i.predicted_dt);
    let predicted=if predicted.is_finite()&&predicted>0.{Duration::from_secs_f32(predicted)}else{Duration::ZERO};
    ctx.request_repaint_after_for(retry+predicted,viewport);
}

#[derive(Clone,Copy,Debug,Serialize,PartialEq,Eq)]
pub(super) struct Snapshot {
    pub inflight:usize,
    pub limit:usize,
    pub producer_completed:u64,
    pub deferred:u64,
    pub polls:u64,
}
#[derive(Clone,Copy,Debug,Serialize,PartialEq,Eq)]
#[serde(rename_all="snake_case")]
pub(super) enum Transition { CapacityDeferred, CapacityResumed }
#[derive(Clone,Copy,Debug)]
pub(super) enum Admission {
    Ready{transition:Option<Transition>},
    Deferred{retry_after:Duration,transition:Option<Transition>},
}
pub(super) struct Leases<T,M=()> {
    queue:VecDeque<(u64,T)>,
    published:Option<(u64,M)>,
    limit:usize,
    completed:u64,
    busy:bool,
    next_poll:Option<Instant>,
    delay:Duration,
    deferred:u64,
    polls:u64,
}
impl<T,M> Leases<T,M> {
    pub fn new(limit:usize)->Self {
        assert!(limit>0);
        Self{queue:VecDeque::new(),published:None,limit,completed:0,busy:false,next_poll:None,delay:FIRST_POLL,deferred:0,polls:0}
    }
    pub fn snapshot(&self)->Snapshot {
        Snapshot{inflight:self.queue.len(),limit:self.limit,producer_completed:self.completed,deferred:self.deferred,polls:self.polls}
    }
    /// A busy callback returns without querying the GPU until the next poll.
    /// New pending images may replace each other without acquiring a lease.
    pub fn pending(&mut self,now:Instant)->Option<Admission> {
        self.next_poll.filter(|deadline|now<*deadline).map(|deadline|{
            self.deferred+=1;
            Admission::Deferred{retry_after:deadline.duration_since(now),transition:None}
        })
    }
    /// Called only after reading the producer fence. MAX denotes device loss
    /// and must be handled before admission; it never proves all leases done.
    pub fn observe(&mut self,completed:u64,now:Instant)->Admission {
        assert_ne!(completed,u64::MAX);
        self.polls+=1;
        let progressed=completed>self.completed;
        self.completed=self.completed.max(completed);
        while self.queue.front().is_some_and(|(value,_)|*value<=self.completed){self.queue.pop_front();}
        if self.queue.len()<self.limit {
            let transition=self.busy.then_some(Transition::CapacityResumed);
            self.busy=false;self.next_poll=None;self.delay=FIRST_POLL;
            return Admission::Ready{transition};
        }
        let transition=(!self.busy).then_some(Transition::CapacityDeferred);
        self.delay=if !self.busy||progressed{FIRST_POLL}else{(self.delay*2).min(MAX_POLL)};
        self.busy=true;self.next_poll=Some(now+self.delay);self.deferred+=1;
        Admission::Deferred{retry_after:self.delay,transition}
    }
    /// Reserve before any conversion submission. A failed conversion retains
    /// its frame until explicit generation retirement, never frees a live read.
    pub fn reserve(&mut self,frame:T) {
        assert!(self.queue.len()<self.limit);
        self.queue.push_back((u64::MAX,frame));
    }
    pub fn produced(&mut self,value:u64,metadata:M) {
        assert!(value>0&&value!=u64::MAX);
        self.queue.back_mut().expect("conversion must own a decoder lease").0=value;
        // The ready fence and exact observation become visible together only
        // after a successful submission. Deferred cannot relabel an old image.
        self.published=Some((value,metadata));
    }
    pub fn published(&self)->Option<(u64,&M)>{self.published.as_ref().map(|(fence,metadata)|(*fence,metadata))}
    pub fn clear(&mut self){self.queue.clear();}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn retry_keeps_a_real_delay_on_the_requested_viewport() {
        use eframe::egui;
        let ctx=egui::Context::default();for _ in 0..3{let _=ctx.run(Default::default(),|_|{});}
        let calls=Arc::new(std::sync::Mutex::new(Vec::new()));let observed=calls.clone();
        ctx.set_request_repaint_callback(move |info|observed.lock().unwrap().push((info.viewport_id,info.delay)));
        request_retry(&ctx,egui::ViewportId::ROOT,FIRST_POLL);
        let calls=calls.lock().unwrap();assert_eq!(calls.len(),1);assert_eq!(calls[0].0,egui::ViewportId::ROOT);
        assert!(calls[0].1>=Duration::from_micros(1999)&&calls[0].1<=Duration::from_micros(2001),"busy admission must not wake immediately: {:?}",calls[0].1);
    }
    #[test]
    fn retry_uses_its_owner_prediction_and_leaves_other_windows_idle() {
        use eframe::egui;
        let ctx=egui::Context::default();let owner=egui::ViewportId::from_hash_of("capacity-owner");
        for _ in 0..3 {
            let mut raw=egui::RawInput{viewport_id:owner,..Default::default()};raw.viewports.entry(owner).or_default();
            let _=ctx.run(raw,|_|{});
        }
        ctx.input_mut_for(egui::ViewportId::ROOT,|i|i.predicted_dt=0.2);
        ctx.input_mut_for(owner,|i|i.predicted_dt=0.04);
        let calls=Arc::new(std::sync::Mutex::new(Vec::new()));let observed=calls.clone();
        ctx.set_request_repaint_callback(move |info|observed.lock().unwrap().push((info.viewport_id,info.delay)));
        request_retry(&ctx,owner,FIRST_POLL);
        assert_eq!(*calls.lock().unwrap(),[(owner,FIRST_POLL)]);
    }
    #[test]
    fn capacity_retains_inflight_and_accepts_latest_after_completion() {
        let now=Instant::now();let mut leases=Leases::new(2);
        let old=Arc::new(10);let weak=Arc::downgrade(&old);
        leases.reserve(old);leases.produced(1,"old");leases.reserve(Arc::new(20));leases.produced(2,"current");
        assert!(matches!(leases.observe(0,now),Admission::Deferred{transition:Some(Transition::CapacityDeferred),..}));
        let mut pending=Arc::new(30);let replaced=Arc::downgrade(&pending);pending=Arc::new(40);
        assert!(replaced.upgrade().is_none(),"latest pending never enters the inflight ledger");
        assert_eq!(leases.snapshot().inflight,2);assert!(weak.upgrade().is_some());
        assert_eq!(leases.published(),Some((2,&"current")),"a deferred latest image must not acquire the old ready fence");
        assert!(matches!(leases.observe(1,now+FIRST_POLL),Admission::Ready{transition:Some(Transition::CapacityResumed)}));
        assert!(weak.upgrade().is_none(),"only the completed producer releases its decoder slot");
        leases.reserve(pending);leases.produced(3,"latest");
        assert_eq!(leases.published(),Some((3,&"latest")));
        assert_eq!(leases.queue.iter().map(|(_,v)|**v).collect::<Vec<_>>(),[20,40]);
    }
    #[test]
    fn repeated_ui_callbacks_do_not_requery_gpu_or_resubmit() {
        let now=Instant::now();let mut leases=Leases::new(1);leases.reserve(1);leases.produced(1,());
        leases.observe(0,now);let initial=leases.snapshot();
        for _ in 0..1000 {assert!(matches!(leases.pending(now),Some(Admission::Deferred{transition:None,..})));}
        assert_eq!(leases.snapshot().polls,initial.polls);assert_eq!(leases.snapshot().inflight,1);
        assert!(leases.pending(now+FIRST_POLL).is_none());
        assert!(matches!(leases.observe(0,now+FIRST_POLL),Admission::Deferred{retry_after,transition:None} if retry_after==Duration::from_millis(4)));
    }
    #[test]
    fn no_progress_poll_is_bounded_and_progress_restarts_admission() {
        let mut now=Instant::now();let mut leases=Leases::new(1);leases.reserve(1);leases.produced(9,());
        for expected in [2,4,8,16,16,16] {
            match leases.observe(0,now) {Admission::Deferred{retry_after,..}=>{assert_eq!(retry_after,Duration::from_millis(expected));now+=retry_after;},_=>panic!()}
        }
        assert!(matches!(leases.observe(9,now),Admission::Ready{transition:Some(Transition::CapacityResumed)}));
        leases.reserve(2);leases.produced(10,());
        assert!(matches!(leases.observe(9,now),Admission::Deferred{retry_after,transition:Some(Transition::CapacityDeferred)} if retry_after==FIRST_POLL));
    }
    #[test]
    fn failed_submission_and_regressing_fence_cannot_release_live_frame() {
        let now=Instant::now();let mut leases:Leases<_,()>=Leases::new(2);let frame=Arc::new(1);let weak=Arc::downgrade(&frame);
        leases.reserve(frame); // Conversion may have queued GPU work before failing.
        assert!(matches!(leases.observe(8,now),Admission::Ready{..}));assert!(weak.upgrade().is_some());
        leases.reserve(Arc::new(2));leases.produced(10,());
        assert!(matches!(leases.observe(3,now),Admission::Deferred{..}));assert_eq!(leases.snapshot().producer_completed,8);
        assert!(weak.upgrade().is_some());
    }
}
