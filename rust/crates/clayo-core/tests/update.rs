//! Port of checks/update.cs: which release is newer, and where its script and page are.
//! Assumes the default update repo (no CLAYO_UPDATE_REPO at build time).

use clayo_core::system::updater::{
    newer, newer_from_json, release_url, script_url, tag_from_json, Version,
};

// C#'s assembly version had four parts (1.0.0.0); release tags have three and a v.
const CURRENT: Version = Version::new(1, 0, 0);

#[test]
fn newer_versions() {
    assert_eq!(
        newer(Some("v1.0.1"), CURRENT),
        Some(Version::new(1, 0, 1)),
        "a later patch"
    );
    assert_eq!(
        newer(Some("v1.2.0"), CURRENT),
        Some(Version::new(1, 2, 0)),
        "a later minor"
    );
    assert_eq!(
        newer(Some("2.0.0"), CURRENT),
        Some(Version::new(2, 0, 0)),
        "without the v too"
    );
}

#[test]
fn not_newer() {
    assert_eq!(newer(Some("v1.0.0"), CURRENT), None, "the same release");
    assert_eq!(
        newer(Some("v1.0.0.0"), CURRENT),
        None,
        "the same release in four parts"
    );
    assert_eq!(newer(Some("v0.9.9"), CURRENT), None, "an older one");
    assert_eq!(
        newer(Some("nightly"), CURRENT),
        None,
        "a tag that is no version"
    );
    assert_eq!(newer(None, CURRENT), None, "no tag");
}

#[test]
fn versions_parse_like_dotnet() {
    assert_eq!(Version::parse("1.0.0.0"), Some(CURRENT));
    assert_eq!(Version::parse("1.0"), Some(CURRENT));
    assert_eq!(Version::parse("1"), None);
    assert_eq!(Version::parse("1.0.x"), None);
    let pkg = env!("CARGO_PKG_VERSION");
    assert_eq!(
        Version::parse(pkg).map(|v| v.to_string()).as_deref(),
        Some(pkg)
    );
}

#[test]
fn script_from_the_tag() {
    assert_eq!(
        script_url(Version::new(1, 0, 2)),
        "https://raw.githubusercontent.com/YogiOnCode/Clayo/v1.0.2/scripts/install.ps1"
    );
}

#[test]
fn release_page_of_the_tag() {
    assert_eq!(
        release_url(Version::new(1, 0, 5)),
        "https://github.com/YogiOnCode/Clayo/releases/tag/v1.0.5"
    );
}

#[test]
fn tag_from_github_json() {
    let body = r#"{"url":"x","tag_name":"v1.0.7","name":"Clayo 1.0.7"}"#;
    assert_eq!(tag_from_json(body).as_deref(), Some("v1.0.7"));
    assert_eq!(newer_from_json(body, CURRENT), Some(Version::new(1, 0, 7)));
    assert_eq!(tag_from_json(r#"{"message":"Not Found"}"#), None);
    assert_eq!(tag_from_json("<html>"), None);
}
