// SPS metadata generated from synthetic 64x64 red frames using FFmpeg/libx264.
// Cases vary profile, color_range (pc/tv), and colorspace (smpte170m/bt709).
// No captured desktop or private account data. No FFmpeg runtime dependency.
pub const CASES: &[(&str, &[u8], &str)] = &[
    (
        "baseline-pc-smpte170m",
        &[
            0, 0, 0, 1, 103, 66, 192, 10, 217, 4, 38, 192, 91, 131, 3, 3, 32, 0, 0, 3, 0, 32, 0, 0,
            3, 0, 65, 226, 68, 201,
        ],
        "Full601",
    ),
    (
        "main-tv-bt709",
        &[
            0, 0, 0, 1, 103, 77, 64, 10, 236, 162, 19, 96, 45, 64, 64, 64, 80, 0, 0, 3, 0, 16, 0,
            0, 3, 0, 32, 241, 34, 89, 96,
        ],
        "Limited709",
    ),
    (
        "high-pc-bt709",
        &[
            0, 0, 0, 1, 103, 100, 0, 10, 172, 217, 68, 38, 192, 91, 128, 128, 128, 160, 0, 0, 3, 0,
            32, 0, 0, 3, 0, 65, 226, 68, 178, 192,
        ],
        "Full709",
    ),
    (
        "high-tv-smpte170m",
        &[
            0, 0, 0, 1, 103, 100, 0, 10, 172, 217, 68, 38, 192, 90, 131, 3, 3, 32, 0, 0, 3, 0, 32,
            0, 0, 3, 0, 65, 226, 68, 178, 192,
        ],
        "Limited601",
    ),
];
