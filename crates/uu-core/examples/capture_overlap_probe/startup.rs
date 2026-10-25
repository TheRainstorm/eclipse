//! Probe startup and first-frame contract used by the real Windows runner.
//! The overlap owner is already configured by Input::new. Serial retains the
//! production caller contract: set the output rate and apply encoder preference.
pub trait Driver {
    type Frame;
    type Error;
    fn set_frame_rate(&mut self, fps: u32) -> Result<(), Self::Error>;
    fn prefer_gpu(&mut self, gpu: bool, vendor: Option<u16>) -> Result<(), Self::Error>;
    fn next_frame(&mut self) -> Result<Self::Frame, Self::Error>;
}
pub fn prepare<D: Driver>(
    input: &mut D,
    overlap: bool,
    fps: u32,
    gpu: bool,
    vendor: Option<u16>,
) -> Result<(), D::Error> {
    if !overlap {
        input.set_frame_rate(fps)?;
    }
    // Same preference is a no-op for the already-configured overlap owner.
    input.prefer_gpu(gpu, vendor)
}
pub fn next<D: Driver>(
    input: &mut D,
    gpu: bool,
    vendor: Option<u16>,
) -> Result<D::Frame, D::Error> {
    // Mirror the production loop, including the very first acquisition.
    input.prefer_gpu(gpu, vendor)?;
    input.next_frame()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug, PartialEq)]
    enum Call {
        Rate(u32),
        Gpu(bool, Option<u16>),
        Frame,
    }
    struct Source {
        gpu: bool,
        vendor: Option<u16>,
        fps: u32,
        calls: Vec<Call>,
        rate_fail: bool,
        gpu_fail: bool,
        overlap_generation: u32,
    }
    impl Source {
        fn new(overlap: bool) -> Self {
            Self {
                gpu: overlap,
                vendor: if overlap { Some(0x1002) } else { None },
                fps: if overlap { 60 } else { 24 },
                calls: vec![],
                rate_fail: false,
                gpu_fail: false,
                overlap_generation: u32::from(overlap),
            }
        }
    }
    impl Driver for Source {
        type Frame = (bool, Option<u16>, u32);
        type Error = &'static str;
        fn set_frame_rate(&mut self, fps: u32) -> Result<(), Self::Error> {
            self.calls.push(Call::Rate(fps));
            if self.rate_fail {
                return Err("rate failed");
            }
            self.fps = fps;
            if self.overlap_generation > 0 {
                self.overlap_generation += 1;
            }
            Ok(())
        }
        fn prefer_gpu(&mut self, gpu: bool, vendor: Option<u16>) -> Result<(), Self::Error> {
            self.calls.push(Call::Gpu(gpu, vendor));
            if self.gpu_fail {
                return Err("preference failed");
            }
            if self.overlap_generation > 0 && (self.gpu, self.vendor) != (gpu, vendor) {
                self.overlap_generation += 1;
            }
            self.gpu = gpu;
            self.vendor = vendor;
            Ok(())
        }
        fn next_frame(&mut self) -> Result<Self::Frame, Self::Error> {
            self.calls.push(Call::Frame);
            Ok((self.gpu, self.vendor, self.fps))
        }
    }
    #[test]
    fn real_prepare_and_first_next_apply_serial_rate_and_gpu() {
        let mut s = Source::new(false);
        prepare(&mut s, false, 60, true, Some(0x1002)).unwrap();
        assert_eq!(
            next(&mut s, true, Some(0x1002)).unwrap(),
            (true, Some(0x1002), 60)
        );
        assert_eq!(
            s.calls,
            vec![
                Call::Rate(60),
                Call::Gpu(true, Some(0x1002)),
                Call::Gpu(true, Some(0x1002)),
                Call::Frame
            ]
        );
    }
    #[test]
    fn real_overlap_first_frame_applies_preference_without_reconfiguration() {
        let mut s = Source::new(true);
        prepare(&mut s, true, 60, true, Some(0x1002)).unwrap();
        assert_eq!(
            next(&mut s, true, Some(0x1002)).unwrap(),
            (true, Some(0x1002), 60)
        );
        assert_eq!(s.overlap_generation, 1);
        assert_eq!(
            s.calls,
            vec![
                Call::Gpu(true, Some(0x1002)),
                Call::Gpu(true, Some(0x1002)),
                Call::Frame
            ]
        );
    }
    #[test]
    fn old_unprepared_serial_first_frame_reproduces_cpu_default() {
        let mut s = Source::new(false);
        assert_eq!(s.next_frame().unwrap(), (false, None, 24));
        let mut fixed = Source::new(false);
        prepare(&mut fixed, false, 60, true, Some(0x1002)).unwrap();
        assert!(next(&mut fixed, true, Some(0x1002)).unwrap().0);
    }
    #[test]
    fn preference_failure_never_acquires_a_frame() {
        let mut s = Source::new(false);
        s.gpu_fail = true;
        assert_eq!(next(&mut s, true, Some(0x1002)), Err("preference failed"));
        assert_eq!(s.calls, vec![Call::Gpu(true, Some(0x1002))]);
    }
    #[test]
    fn rate_failure_stops_before_preference_or_acquisition() {
        let mut s = Source::new(false);
        s.rate_fail = true;
        assert_eq!(
            prepare(&mut s, false, 60, true, Some(0x1002)),
            Err("rate failed")
        );
        assert_eq!(s.calls, vec![Call::Rate(60)]);
    }
    #[test]
    fn next_never_fakes_the_returned_gpu_status() {
        let mut s = Source::new(false);
        prepare(&mut s, false, 60, false, None).unwrap();
        assert!(!next(&mut s, false, None).unwrap().0);
    }
}
