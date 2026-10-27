//! Provider-neutral cursor shape.
#[derive(Clone, Debug)]
pub struct Shape {
    pub hotspot: [i32; 2],
    pub size: [u32; 2],
    pub image: Vec<u8>,
    pub kind: i32,
    pub scale: [f64; 2],
    pub screen: i32,
}
