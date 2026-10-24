//! SDL's logical buttons/down-positive axes to an Xbox 360 report.
use super::State;
pub(super) struct Report {pub buttons:u16,pub triggers:[u8;2],pub sticks:[i16;4]}
pub(super) fn report(state:&State)->Report {
    let mut buttons=0;
    for (source,target) in [(0,0x1000),(1,0x2000),(2,0x4000),(3,0x8000),(4,0x20),(5,0x400),(6,0x10),(7,0x40),(8,0x80),(9,0x100),(10,0x200),(11,1),(12,2),(13,4),(14,8)] {
        if state.buttons&(1<<source)!=0{buttons|=target;}
    }
    let mut sticks=state.sticks;sticks[1]=sticks[1].saturating_neg();sticks[3]=sticks[3].saturating_neg();
    Report{buttons,triggers:state.triggers.map(|value|(u32::from(value)*255/65535) as u8),sticks}
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn xbox_buttons_axes_and_triggers_preserve_portable_coordinates(){
        let state=State{buttons:1|1<<4|1<<5|1<<9|1<<11,sticks:[i16::MIN,i16::MIN,i16::MAX,i16::MAX],triggers:[0,u16::MAX],..Default::default()};
        let report=report(&state);assert_eq!(report.buttons,0x1000|0x20|0x400|0x100|1);
        assert_eq!(report.triggers,[0,255]);assert_eq!(report.sticks,[i16::MIN,i16::MAX,i16::MAX,-i16::MAX]);
        let neutral=super::report(&State::default());assert_eq!(neutral.buttons,0);assert_eq!(neutral.sticks,[0;4]);assert_eq!(neutral.triggers,[0;2]);
    }
}
