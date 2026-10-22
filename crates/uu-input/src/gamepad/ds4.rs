//! USB report layout and inverse calibration, from the attributed public data.
use super::State;
pub const USB_CALIBRATION: &[u8;37]=include_bytes!("../../../../vendor/ds4-data/usb-calibration.bin");
pub const VIGEM_CALIBRATION: &[u8;37]=include_bytes!("../../../../vendor/ds4-data/vigem-calibration.bin");
fn value(bytes:&[u8],at:usize)->f32 {i16::from_le_bytes([bytes[at],bytes[at+1]]) as f32}
fn put(bytes:&mut[u8],at:usize,v:f32){bytes[at..at+2].copy_from_slice(&(v.round().clamp(i16::MIN as f32,i16::MAX as f32) as i16).to_le_bytes());}
/// The calibration report defines the backend's raw units. Linux's synthetic
/// device and ViGEmBus use different calibration; do not reuse hardcoded gains.
pub fn report(state:&State,calibration:&[u8;37])->[u8;64] {
    let mut data=[0;64];data[0]=1;
    for (at,axis) in state.sticks.iter().enumerate(){data[at+1]=((i32::from(*axis)+32768)*256/65536).min(255) as u8;}
    let b=|i:u32|state.buttons&(1u32<<i)!=0;
    let horizontal=i32::from(b(14))-i32::from(b(13));let vertical=i32::from(b(12))-i32::from(b(11));
    data[5]=match(horizontal,vertical){(0,-1)=>0,(1,-1)=>1,(1,0)=>2,(1,1)=>3,(0,1)=>4,(-1,1)=>5,(-1,0)=>6,(-1,-1)=>7,_=>8};
    for (button,flag) in [(2,0x10),(0,0x20),(1,0x40),(3,0x80)]{if b(button){data[5]|=flag;}}
    for (button,flag) in [(9,1),(10,2),(4,0x10),(6,0x20),(7,0x40),(8,0x80)]{if b(button){data[6]|=flag;}}
    if state.triggers[0]>0 {data[6]|=4;}if state.triggers[1]>0{data[6]|=8;}
    data[7]=u8::from(b(5))|(u8::from(b(20))<<1)|((((state.timestamp_us/8000)&63) as u8)<<2);
    data[8]=(u32::from(state.triggers[0])*255/65535) as u8;data[9]=(u32::from(state.triggers[1])*255/65535) as u8;
    data[10..12].copy_from_slice(&((u128::from(state.timestamp_us)*3/16) as u16).to_le_bytes());data[12]=255;
    let gyro=state.gyro.unwrap_or([0.;3]);let accel=state.acceleration.unwrap_or([0.,9.80665,0.]);
    let speed=value(calibration,19)+value(calibration,21);
    for axis in 0..3 {
        let bias=value(calibration,1+axis*2);
        let denominator=(value(calibration,7+axis*4)-bias).abs()+(value(calibration,9+axis*4)-bias).abs();
        put(&mut data,13+axis*2,bias+gyro[axis].to_degrees()*denominator/speed);
        let plus=value(calibration,23+axis*4);let minus=value(calibration,25+axis*4);
        put(&mut data,19+axis*2,(plus+minus)/2.+accel[axis]/9.80665*(plus-minus)/2.);
    }
    data[30]=0x1a;data[33]=1;data[35]=0x80;data[39]=0x80;
    data
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn sensor_values_roundtrip_through_both_backends_calibration_and_timestamp_wrap(){
        for cal in [USB_CALIBRATION,VIGEM_CALIBRATION] {
            let state=State{buttons:1|1<<11,sticks:[i16::MIN,i16::MAX,0,0],triggers:[65535,0],timestamp_us:1_000_000,gyro:Some([0.3,-0.5,1.]),acceleration:Some([1.,9.80665,-2.]),..State::default()};
            let data=report(&state,cal);
            assert_eq!(&data[1..3],&[0,255]);assert_eq!(data[5],0x20);assert_eq!(data[8],255);
            assert_eq!(u16::from_le_bytes([data[10],data[11]]),187500u32 as u16);
            for axis in 0..3 {
                let bias=value(cal,1+axis*2);let denominator=(value(cal,7+axis*4)-bias).abs()+(value(cal,9+axis*4)-bias).abs();
                let gyro=(value(&data,13+axis*2)-bias)*(value(cal,19)+value(cal,21))/denominator;
                assert!((gyro.to_radians()-state.gyro.unwrap()[axis]).abs()<0.002);
                let plus=value(cal,23+axis*4);let minus=value(cal,25+axis*4);
                let accel=(value(&data,19+axis*2)-(plus+minus)/2.)*2.*9.80665/(plus-minus);
                assert!((accel-state.acceleration.unwrap()[axis]).abs()<0.003);
            }
        }
    }
}
