//! Shared media codec and bitstream identifiers, independent of connection providers.
use serde::{Serialize,Deserialize};
/// Supported standard codecs and the explicitly negotiated experimental format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    #[default]
    H264,
    Hevc,
    Av1,
    /// Eclipse native transport only, explicitly negotiated C API/bitstream v1.
    Pyrowave,
}
pub const PYROWAVE_FMTP:&str="eclipse-pyrowave=1;upstream=d7a03df74bff;api=1.0.0;chroma=420;color=bt709-full;depth=8";
