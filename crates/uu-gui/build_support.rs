//! Pure build helpers, also exercised by the GUI's unit tests.
#[cfg_attr(not(test), allow(dead_code))]
pub fn windows_version(version: &str) -> Result<[u16; 3], &'static str> {
    let base = version.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<_> = base.split('.').collect();
    if parts.len() != 3 {
        return Err("Windows resource version needs three numeric components");
    }
    let mut numbers = [0; 3];
    for (out, part) in numbers.iter_mut().zip(parts) {
        *out = part
            .parse()
            .map_err(|_| "Windows version component exceeds 65535 or is not numeric")?;
    }
    Ok(numbers)
}

pub fn valid_revision(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_resources_accept_prerelease_and_build_metadata() {
        for version in [
            "0.6.0",
            "0.6.0-dev.1",
            "0.6.0+build.42",
            "0.6.0-rc.1+build.42",
        ] {
            assert_eq!(super::windows_version(version), Ok([0, 6, 0]));
        }
        assert_eq!(super::windows_version("1.2.65535"), Ok([1, 2, 65535]));
        for version in ["1.2", "1.2.3.4", "1.2.65536", "1.2.bad"] {
            assert!(super::windows_version(version).is_err());
        }
    }
}
