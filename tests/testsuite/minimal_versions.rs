//! Tests for minimal-version resolution.
//!
//! Note: Some tests are located in the resolver-tests package.

use cargo_test_support::project;
use cargo_test_support::registry::Package;

// Ensure that the "-Z minimal-versions" CLI option works and the minimal
// version of a dependency ends up in the lock file.
#[cargo_test]
fn minimal_version_cli() {
    Package::new("dep", "1.0.0").publish();
    Package::new("dep", "1.1.0").publish();

    let p = project()
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "foo"
                authors = []
                version = "0.0.1"

                [dependencies]
                dep = "1.0"
            "#,
        )
        .file("src/main.rs", "fn main() {}")
        .build();

    p.cargo("generate-lockfile -Zminimal-versions")
        .masquerade_as_nightly_cargo()
        .run();

    let lock = p.read_lockfile();

    assert!(!lock.contains("1.1.0"));
}

#[cargo_test]
fn same_version_different_metadata() {
    Package::new("dep", "1.0.0+build1").publish();
    Package::new("dep", "1.0.0+build2").publish();

    enum Dep {
        Build1,
        Build2,
    }

    for (req, expected_resolve) in &[
        ("1.0.0", Dep::Build1),
        ("1.0.0+irrelevant", Dep::Build1),
        ("1.0.0+build2", Dep::Build1),
        ("=1.0.0+build2", Dep::Build2),
    ] {
        let p = project()
            .file(
                "Cargo.toml",
                &format!(
                    r#"
                        [package]
                        name = "foo"
                        version = "0.0.0"

                        [dependencies]
                        dep = "{}"
                    "#,
                    req,
                ),
            )
            .file("src/main.rs", "fn main() {}")
            .build();

        p.cargo("generate-lockfile -Zminimal-versions")
            .masquerade_as_nightly_cargo()
            .run();

        let lock = p.read_lockfile();

        match expected_resolve {
            Dep::Build1 => {
                assert!(lock.contains("1.0.0+build1"));
                assert!(!lock.contains("1.0.0+build2"));
            }
            Dep::Build2 => {
                assert!(!lock.contains("1.0.0+build1"));
                assert!(lock.contains("1.0.0+build2"));
            }
        }
    }
}
