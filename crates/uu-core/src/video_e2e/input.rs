//! An actual input's delivery and a subsequent observed desktop image.
//! Post-injection source ordering is evidence of pipeline progress, never
//! evidence that an arbitrary application has responded to that input.
use super::*;
use std::time::Instant;

const INPUT_LIMIT: usize = 32;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Delay {
    pub samples: u32,
    pub mean_us: u64,
    /// Bound on this batch's mean, not a standard deviation.
    pub uncertainty_us: u64,
    pub p95_us: u64,
    /// Variation of interval midpoints, kept separate from clock uncertainty.
    pub std_dev_us: u64,
}
impl Delay {
    fn from_intervals(intervals: impl IntoIterator<Item = TimeInterval>) -> Option<Self> {
        let intervals: Vec<_> = intervals.into_iter().collect();
        if intervals.is_empty() || intervals.len() > INPUT_LIMIT
            || intervals.iter().any(|v| v.earliest_ns > v.latest_ns || v.latest_ns > 10_000_000_000)
        { return None; }
        let n = intervals.len() as u128;
        let lower: u128 = intervals.iter().map(|v| v.earliest_ns).sum();
        let upper: u128 = intervals.iter().map(|v| v.latest_ns).sum();
        let mean = (lower + (upper-lower)/2)/(n*1000);
        let uncertainty = lower.abs_diff(mean*n*1000).max(upper.abs_diff(mean*n*1000)).div_ceil(n*1000);
        let mut centers: Vec<_> = intervals.iter().map(|v| (v.earliest_ns+(v.latest_ns-v.earliest_ns)/2) as f64/1000.).collect();
        let center_mean = centers.iter().sum::<f64>()/n as f64;
        let deviation = (centers.iter().map(|v| (v-center_mean).powi(2)).sum::<f64>()/n as f64).sqrt();
        centers.sort_by(f64::total_cmp);
        Some(Self { samples:n as u32, mean_us:mean as u64, uncertainty_us:uncertainty as u64,
            p95_us:centers[(centers.len()*95).div_ceil(100)-1].round() as u64, std_dev_us:deviation.round() as u64 })
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Snapshot {
    pub generation: u64,
    pub tracked_inputs: u32,
    /// Client enqueue through CONTROL receive callback, including client wait.
    pub enqueue_to_host: Option<Delay>,
    /// Client enqueue through successful OS input injection.
    pub enqueue_to_injected: Option<Delay>,
    /// Same-host wait until a subsequently presented, observed source image.
    /// This can include application work, but does not identify its response.
    pub injected_to_source: Option<Delay>,
    /// Same-client enqueue to the source image's existing first-display proof.
    /// Missing source/display evidence stays unknown; physical scanout excluded.
    pub input_to_frame: Option<Measurement>,
}

pub(super) struct Receipt {
    enqueued: TimeInterval,
    received: WireClockStamp,
    injected: WireClockStamp,
    acknowledged: WireClockStamp,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) struct Enqueued {
    pub interval: TimeInterval,
    pub generation: u64,
    pub observation_epoch: u64,
}

fn elapsed(start: TimeInterval, end: TimeInterval) -> Option<TimeInterval> {
    if end.latest_ns < start.earliest_ns { return None; }
    let interval = TimeInterval { earliest_ns:end.earliest_ns.saturating_sub(start.latest_ns),
        latest_ns:end.latest_ns-start.earliest_ns };
    (interval.latest_ns<=10_000_000_000).then_some(interval)
}
fn same_clock(a: WireClockStamp, b: WireClockStamp) -> bool {
    a.epoch != 0 && a.epoch == b.epoch && a.frequency_hz != 0 && a.frequency_hz == b.frequency_hz
}

impl Host {
    pub(crate) fn input_clock_stamp(&self) -> Option<WireClockStamp> { self.clock.stamp() }
}
impl Client {
    /// Bracket the counter read with Instants instead of equating clock origins.
    pub(crate) fn input_enqueued(&self, created: Instant) -> Option<Enqueued> {
        let generation=self.generation.load(Ordering::Acquire);
        let observation_epoch=self.observation_epoch.load(Ordering::Acquire);
        let before = Instant::now();
        if created > before { return None; }
        let stamp = self.local.stamp()?.interval_ns()?;
        let after = Instant::now();
        if generation!=self.generation.load(Ordering::Acquire) || observation_epoch!=self.observation_epoch.load(Ordering::Acquire) {return None;}
        Some(Enqueued { interval:TimeInterval { earliest_ns:stamp.earliest_ns.checked_sub(after.duration_since(created).as_nanos())?,
            latest_ns:stamp.latest_ns.checked_sub(before.duration_since(created).as_nanos())? },generation,observation_epoch })
    }
    pub(crate) fn input_applied(&self, epoch:u64, proof:Enqueued, received:WireClockStamp, injected:WireClockStamp) {
        let enqueued=proof.interval;
        let Some(acknowledged) = self.local.stamp() else { return; };
        let Some(now) = acknowledged.interval_ns() else { return; };
        if !self.supported() || !same_clock(received,injected) || received.ticks==0
            || injected.ticks<received.ticks || enqueued.earliest_ns>enqueued.latest_ns
            || now.latest_ns<enqueued.latest_ns || now.latest_ns-enqueued.earliest_ns>WINDOW.as_nanos()
        { return; }
        let mut state = self.state.lock().unwrap();
        if state.media_epoch!=epoch || state.host_epoch!=Some(received.epoch)
            || state.generation!=proof.generation || self.generation.load(Ordering::Acquire)!=proof.generation
            || state.observation_epoch!=proof.observation_epoch || self.observation_epoch.load(Ordering::Acquire)!=proof.observation_epoch
        { return; }
        state.inputs.retain(|v| now.latest_ns.saturating_sub(v.enqueued.earliest_ns)<WINDOW.as_nanos());
        if state.inputs.len()==INPUT_LIMIT { state.inputs.pop_front(); }
        state.inputs.push_back(Receipt { enqueued,received,injected,acknowledged });
    }
    pub fn input_latency(&self) -> Snapshot {
        let Some(now) = self.local.stamp() else { return Snapshot::default(); };
        let mut state = self.state.lock().unwrap();
        self.drain_metadata(&mut state);
        Self::join_all(&mut state,now);
        let active = self.supported() && state.observation_epoch==self.observation_epoch.load(Ordering::Acquire)
            && self.active_surface.load(Ordering::Acquire)!=0
            && Some(self.active_surface.load(Ordering::Acquire))==state.surface_epoch;
        snapshot(&state,now,active)
    }
}

fn snapshot(state:&State, now:WireClockStamp, display_active:bool) -> Snapshot {
    let Some(now_ns) = now.interval_ns() else { return Snapshot::default(); };
    let mut arrivals = Vec::new(); let mut injections = Vec::new();
    let mut source_waits = Vec::new(); let mut responses = Vec::new();
    let mut count = 0;
    for input in &state.inputs {
        if now_ns.latest_ns<input.enqueued.latest_ns || now_ns.latest_ns-input.enqueued.earliest_ns>=WINDOW.as_nanos()
            || !same_clock(now,input.acknowledged) || state.host_epoch!=Some(input.received.epoch)
        { continue; }
        count+=1;
        let capped = |host| {
            let mut end = state.clock.map_host(host,now)?;
            end.latest_ns=end.latest_ns.min(input.acknowledged.interval_ns()?.latest_ns);
            (end.earliest_ns<=end.latest_ns).then_some(end).and_then(|end|elapsed(input.enqueued,end))
        };
        if let Some(interval)=capped(input.received) { arrivals.push(interval); }
        if let Some(interval)=capped(input.injected) { injections.push(interval); }
        if !display_active { continue; }
        // Choose the earliest *observed and display-qualified* source after
        // this injection. Missing telemetry cannot prove it was the first real
        // source update, nor that its application consumed the input.
        let next = state.samples.iter().filter(|sample| {
            Some(sample.method)==state.method && same_clock(sample.source_presented,input.injected)
                && sample.source_presented.ticks>input.injected.ticks
                && now_ns.latest_ns>=sample.display_ns
                && now_ns.latest_ns-sample.display_ns<WINDOW.as_nanos()
        }).min_by_key(|sample|sample.source_presented.ticks);
        if let Some(sample)=next {
            let display = sample.first.display;
            let (Some(first),Some(last))=(display.earliest.interval_ns(),display.latest.interval_ns()) else {continue;};
            let Some(response)=elapsed(input.enqueued,TimeInterval{earliest_ns:first.earliest_ns,latest_ns:last.latest_ns}) else {continue;};
            let Some(wait)=elapsed(input.injected.interval_ns().unwrap(),sample.source_presented.interval_ns().unwrap()) else {continue;};
            responses.push(response); source_waits.push(wait);
        }
    }
    Snapshot { generation:state.generation, tracked_inputs:count, enqueue_to_host:Delay::from_intervals(arrivals),
        enqueue_to_injected:Delay::from_intervals(injections), injected_to_source:Delay::from_intervals(source_waits),
        input_to_frame:state.method.and_then(|method|Measurement::from_intervals(responses,method)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stamp(epoch:u64,ms:u64)->WireClockStamp { WireClockStamp{epoch,ticks:ms*1000,frequency_hz:1_000_000} }
    fn interval(lo:u128,hi:u128)->TimeInterval { TimeInterval{earliest_ns:lo*1_000_000,latest_ns:hi*1_000_000} }
    fn image(source_ms:u64,display_ms:u64)->Sample {
        let source=stamp(9,source_ms);
        Sample {source_id:(9,1,source_ms),source_presented:source,
            first:FirstDisplay{token:FrameToken{media_epoch:7,observation_epoch:1,track:0,ssrc:42,rtp_timestamp:source_ms as u32},
                display:DisplayObservation{surface_epoch:1,present_id:source_ms,earliest:stamp(5,display_ms),latest:stamp(5,display_ms+2),method:DisplayMethod::PlatformDisplayed}},
            display_ns:display_ms as u128*1_000_000,interval:interval(10,12),method:DisplayMethod::PlatformDisplayed }
    }
    fn state()->State {
        let mut state=State{host_epoch:Some(9),method:Some(DisplayMethod::PlatformDisplayed),surface_epoch:Some(1),..State::default()};
        assert!(state.clock.sample(FourTimestamp{client_send:stamp(5,90),host_receive:stamp(9,1091),host_send:stamp(9,1091),client_receive:stamp(5,92)}));
        state.inputs.push_back(Receipt{enqueued:interval(100,100),received:stamp(9,1103),injected:stamp(9,1105),acknowledged:stamp(5,108)});
        state
    }
    #[test]
    fn post_injection_picture_is_distinct_from_video_latency_and_requires_display_evidence() {
        let mut state=state();
        // A source rendered before injection cannot be the subsequent image,
        // even when its delayed display happens after input enqueue.
        state.samples.push_back(image(1104,125));
        assert!(snapshot(&state,stamp(5,200),true).input_to_frame.is_none());
        state.samples.push_back(image(1112,130));
        let report=snapshot(&state,stamp(5,200),true);
        assert_eq!(report.tracked_inputs,1);
        let frame=report.input_to_frame.unwrap();
        assert_eq!((frame.samples,frame.mean_us,frame.uncertainty_us),(1,31_000,1000));
        assert_eq!(report.injected_to_source.unwrap().mean_us,7000);
        let received=report.enqueue_to_host.unwrap();
        assert!(received.mean_us.abs_diff(3000)<10 && received.uncertainty_us>1000);
        let no_display=snapshot(&state,stamp(5,200),false);
        assert!(no_display.enqueue_to_injected.is_some());
        assert!(no_display.input_to_frame.is_none());
        // Retired/missing proof cannot be replaced with an API render time.
        state.samples.clear();
        assert!(snapshot(&state,stamp(5,200),true).injected_to_source.is_none());
    }
    #[test]
    fn mismatched_domains_stale_inputs_and_clock_uncertainty_are_not_exact_delays() {
        let mut state=state();
        state.samples.push_back(image(1112,130));
        state.samples[0].source_presented.epoch=10;
        assert!(snapshot(&state,stamp(5,200),true).input_to_frame.is_none());
        state.clock.reset();
        let no_clock=snapshot(&state,stamp(5,200),true);
        assert!(no_clock.enqueue_to_host.is_none() && no_clock.enqueue_to_injected.is_none());
        assert_eq!(snapshot(&state,stamp(6,200),true).tracked_inputs,0);
        assert_eq!(snapshot(&state,stamp(5,3200),true).tracked_inputs,0);
        let equal=Delay::from_intervals([interval(2,4),interval(2,4)]).unwrap();
        assert_eq!((equal.mean_us,equal.uncertainty_us,equal.std_dev_us),(3000,1000,0));
        assert!(Delay::from_intervals([interval(4,2)]).is_none());
    }
    #[test]
    fn late_ack_cannot_restore_input_proof_after_observer_or_coverage_retirement() {
        let client=Client::default();client.begin(7,true);
        client.state.lock().unwrap().host_epoch=Some(9);
        let old=client.input_enqueued(Instant::now()).unwrap();
        assert!(client.set_display_observer(7,1,DisplayMethod::PlatformDisplayed));
        client.input_applied(7,old,stamp(9,100),stamp(9,101));
        assert!(client.state.lock().unwrap().inputs.is_empty());
        let fresh=client.input_enqueued(Instant::now()).unwrap();
        client.input_applied(7,fresh,stamp(9,100),stamp(9,101));
        assert_eq!(client.state.lock().unwrap().inputs.len(),1);
        client.retire_metadata(client.observation_epoch.load(Ordering::Acquire));
        client.input_applied(7,fresh,stamp(9,100),stamp(9,101));
        let mut state=client.state.lock().unwrap();client.sync_coverage(&mut state);
        assert!(state.inputs.is_empty());
    }
}
