//! Build provenance comes from this binary; the adjacent package manifest is a cross-check.
use std::{fs::File, io::Read, path::Path};

#[path = "../build_support.rs"]
mod support;

#[derive(Debug, PartialEq)]
enum Manifest {
    Absent,
    Revision(String),
    Invalid,
}

fn manifest(path: &Path) -> Manifest {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Manifest::Absent,
        Err(_) => return Manifest::Invalid,
    };
    let mut data = Vec::new();
    if file.take(64 * 1024 + 1).read_to_end(&mut data).is_err() || data.len() > 64 * 1024 {
        return Manifest::Invalid;
    }
    serde_json::from_slice::<serde_json::Value>(&data)
        .ok()
        .and_then(|value| value["source_commit"].as_str().map(str::to_owned))
        .filter(|revision| support::valid_revision(revision))
        .map(Manifest::Revision)
        .unwrap_or(Manifest::Invalid)
}

fn describe(compiled: &str, dirty: &str, manifest: Manifest) -> String {
    let compiled = support::valid_revision(compiled).then_some(compiled);
    let mut label = match (compiled, &manifest) {
        (Some(revision), _) => revision[..12].to_owned(),
        (None, Manifest::Revision(revision)) => format!("{}（包清单）", &revision[..12]),
        _ => "开发构建（无 Git 信息）".to_owned(),
    };
    if compiled.is_some() {
        match dirty {
            "true" => label.push_str(" · 本地修改"),
            "false" => {}
            _ => label.push_str(" · 工作区状态未知"),
        }
    }
    match manifest {
        Manifest::Revision(revision) if compiled.is_some_and(|actual| actual != revision) => {
            label.push_str(" · 安装清单与程序不一致");
        }
        Manifest::Invalid => label.push_str(" · 安装清单无效"),
        _ => {}
    }
    label
}

pub fn label() -> String {
    let manifest = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.join("version.json")))
        .map(|path| manifest(&path))
        .unwrap_or(Manifest::Absent);
    describe(env!("UU_BUILD_COMMIT"), env!("UU_BUILD_DIRTY"), manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn binary_revision_wins_over_stale_manifest() {
        assert_eq!(
            describe(A, "false", Manifest::Revision(B.into())),
            "aaaaaaaaaaaa · 安装清单与程序不一致"
        );
        assert_eq!(
            describe(A, "false", Manifest::Revision(A.into())),
            "aaaaaaaaaaaa"
        );
    }

    #[test]
    fn local_and_archive_builds_are_distinguishable() {
        assert_eq!(
            describe(A, "true", Manifest::Absent),
            "aaaaaaaaaaaa · 本地修改"
        );
        assert_eq!(
            describe(A, "unknown", Manifest::Absent),
            "aaaaaaaaaaaa · 工作区状态未知"
        );
        assert_eq!(
            describe("", "unknown", Manifest::Revision(A.into())),
            "aaaaaaaaaaaa（包清单）"
        );
        assert_eq!(
            describe("", "unknown", Manifest::Absent),
            "开发构建（无 Git 信息）"
        );
        assert!(
            describe("日".repeat(40).as_str(), "false", Manifest::Invalid).contains("安装清单无效")
        );
    }

    #[test]
    fn malformed_and_oversized_manifests_are_reported_without_echoing_contents() {
        let dir = std::env::temp_dir().join(format!("uu-build-info-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("version.json");
        assert_eq!(manifest(&path), Manifest::Absent);
        for data in [
            b"not json".to_vec(),
            br#"{"source_commit":"private-untrusted-text"}"#.to_vec(),
            vec![b' '; 64 * 1024 + 1],
        ] {
            std::fs::write(&path, data).unwrap();
            assert_eq!(manifest(&path), Manifest::Invalid);
        }
        std::fs::write(&path, format!(r#"{{"source_commit":"{A}"}}"#)).unwrap();
        assert_eq!(manifest(&path), Manifest::Revision(A.into()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
