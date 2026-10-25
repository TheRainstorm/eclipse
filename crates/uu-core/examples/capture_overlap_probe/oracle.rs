//! Pure probe oracle: capture references are RGB; converter readbacks are BGRA.
pub const MAX_RGB_ERROR: u8 = 18;
#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub overlap: bool,
    pub separate: bool,
    pub monitor: Option<String>,
}
pub fn options(args: &[String]) -> Result<Options, String> {
    let values = args.iter().map(String::as_str).collect::<Vec<_>>();
    let (mode, monitor) = match values.as_slice() {
        ["--mode", mode] => (*mode, None),
        ["--mode", mode, "--monitor", name] if !name.is_empty() && !name.starts_with("--") => {
            (*mode, Some((*name).to_owned()))
        }
        _ => {
            return Err(
                "required: --mode serial|overlap|separate [--monitor NAME]; no other arguments".into(),
            )
        }
    };
    let overlap = match mode {
        "serial" => false,
        "overlap" | "separate" => true,
        _ => return Err("mode must be serial, overlap or separate".into()),
    };
    Ok(Options { overlap, separate:mode=="separate", monitor })
}
pub fn bgra_to_rgb(v: [u8; 4]) -> [u8; 3] {
    [v[2], v[1], v[0]]
}
pub fn marker(samples: &[[u8; 3]]) -> Result<u16, String> {
    if samples.len() != 32 {
        return Err("marker width missing".into());
    }
    let mut code = 0u32;
    for (bit, rgb) in samples.iter().enumerate() {
        let level = rgb.iter().map(|v| u32::from(*v)).sum::<u32>() / 3;
        if !(level < 80 || level > 180) {
            return Err(format!("ambiguous marker bit {bit}: {level}"));
        }
        if level > 180 {
            code |= 1 << bit;
        }
    }
    let low = code as u16;
    if (code >> 16) as u16 != !low {
        return Err("marker complement invalid".into());
    }
    Ok(low)
}
#[derive(Debug)]
pub struct Assessment {
    pub actual_rgb: Vec<[u8; 3]>,
    pub point_errors: Vec<[u8; 3]>,
    pub max_error: u8,
    pub expected_marker: Result<u16, String>,
    pub actual_marker: Result<u16, String>,
}
impl Assessment {
    pub fn check(&self) -> Result<(), String> {
        let expected = *self.expected_marker.as_ref().map_err(Clone::clone)?;
        let actual = *self.actual_marker.as_ref().map_err(Clone::clone)?;
        if expected != actual {
            return Err(format!(
                "decoded marker/PTS mismatch: {actual} vs {expected}"
            ));
        }
        if self.max_error > MAX_RGB_ERROR {
            return Err(format!(
                "decoded pixel identity mismatch: max RGB error {}",
                self.max_error
            ));
        }
        Ok(())
    }
}
pub fn assess(expected: &[[u8; 3]], actual_bgra: &[[u8; 4]]) -> Result<Assessment, String> {
    if expected.len() != 38 || actual_bgra.len() != expected.len() {
        return Err("six color and 32 marker points required".into());
    }
    let actual_rgb = actual_bgra
        .iter()
        .copied()
        .map(bgra_to_rgb)
        .collect::<Vec<_>>();
    let point_errors = expected
        .iter()
        .zip(&actual_rgb)
        .map(|(a, b)| std::array::from_fn(|i| a[i].abs_diff(b[i])))
        .collect::<Vec<[u8; 3]>>();
    let max_error = point_errors.iter().flatten().copied().max().unwrap_or(0);
    Ok(Assessment {
        expected_marker: marker(&expected[6..]),
        actual_marker: marker(&actual_rgb[6..]),
        actual_rgb,
        point_errors,
        max_error,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn bits(value: u16) -> Vec<[u8; 3]> {
        let code = u32::from(value) | (u32::from(!value) << 16);
        (0..32)
            .map(|b| [if code & (1 << b) != 0 { 240 } else { 16 }; 3])
            .collect()
    }
    fn reference() -> Vec<[u8; 3]> {
        [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [231, 79, 17],
            [12, 77, 193],
            [128, 128, 128],
        ]
        .into_iter()
        .chain(bits(0x5a39))
        .collect()
    }
    fn bgra(rgb: &[[u8; 3]]) -> Vec<[u8; 4]> {
        rgb.iter().map(|v| [v[2], v[1], v[0], 255]).collect()
    }
    #[test]
    fn explicit_modes_only() {
        assert!(
            !options(&["--mode".into(), "serial".into()])
                .unwrap()
                .overlap
        );
        assert!(
            options(&["--mode".into(), "overlap".into()])
                .unwrap()
                .overlap
        );
        for v in [
            vec![],
            vec!["serial"],
            vec!["--mode", "auto"],
            vec!["--mode", "overlap", "extra"],
        ] {
            assert!(options(&v.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err());
        }
    }
    #[test]
    fn worker_appended_monitor_is_pinned_without_loosening_mode_or_extra_args() {
        let values = |ss: &[&str]| ss.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            options(&values(&[
                "--mode",
                "serial",
                "--monitor",
                r"\\.\DISPLAY13"
            ]))
            .unwrap(),
            Options {
                overlap: false,
                separate:false,
                monitor: Some(r"\\.\DISPLAY13".into())
            }
        );
        assert!(
            options(&values(&["--mode", "overlap", "--monitor", "display"]))
                .unwrap()
                .overlap
        );
        for args in [
            vec!["--mode", "auto", "--monitor", "display"],
            vec!["--mode", "serial", "--monitor", ""],
            vec!["--mode", "serial", "--monitor", "--bad"],
            vec!["--mode", "serial", "--monitor", "display", "extra"],
            vec!["--mode", "serial", "--monitor"],
        ] {
            assert!(options(&values(&args)).is_err());
        }
    }
    #[test]
    fn known_colors_asymmetric_values_and_gray_use_bgra_contract() {
        let r = reference();
        let a = assess(&r, &bgra(&r)).unwrap();
        a.check().unwrap();
        assert_eq!(a.actual_rgb, r);
        assert_eq!(a.max_error, 0);
        assert_eq!(bgra_to_rgb([17, 79, 231, 53]), [231, 79, 17]);
    }
    #[test]
    fn swapped_red_blue_is_rejected_while_gray_markers_still_match() {
        let r = reference();
        let wrong = r
            .iter()
            .map(|v| [v[0], v[1], v[2], 255])
            .collect::<Vec<_>>();
        let a = assess(&r, &wrong).unwrap();
        assert_eq!(a.expected_marker, a.actual_marker);
        assert_eq!(a.max_error, 255);
        assert!(a.check().is_err());
    }
    #[test]
    fn original_rgb_error_boundary_is_preserved() {
        let r = reference();
        let mut actual = bgra(&r);
        actual[5][0] += 18;
        assess(&r, &actual).unwrap().check().unwrap();
        actual[5][0] += 1;
        assert!(assess(&r, &actual).unwrap().check().is_err());
    }
    #[test]
    fn complement_identity_and_ambiguous_marker_bits_are_rejected() {
        let mut r = reference();
        let mut actual = bgra(&r);
        for (v, b) in actual[6..].iter_mut().zip(bgra(&bits(0x5a38))) {
            *v = b;
        }
        assert!(assess(&r, &actual)
            .unwrap()
            .check()
            .unwrap_err()
            .contains("marker/PTS"));
        r[6] = [16; 3];
        assert!(marker(&r[6..]).is_err());
        let mut b = bits(3);
        b[0] = [100; 3];
        assert!(marker(&b).unwrap_err().contains("ambiguous"));
        assert!(marker(&b[..31]).is_err());
        for v in [0, 1, 0x5a39, 65535] {
            assert_eq!(marker(&bits(v)), Ok(v));
        }
    }
}

#[cfg(test)] mod separate_tests {use super::*;
    #[test] fn separate_mode_requires_explicit_exact_name(){let v=|x:&str|vec!["--mode".to_owned(),x.to_owned()];let s=options(&v("separate")).unwrap();assert!(s.overlap&&s.separate);for old in ["serial","overlap"]{assert!(!options(&v(old)).unwrap().separate);}assert!(options(&v("separate1")).is_err());}
}
