//! Tests for the `cargo vendor` command.
//!
//! Note that every test here uses `--respect-source-config` so that the
//! "fake" crates.io is used. Otherwise `vendor` would download the crates.io
//! index from the network.

use std::fs;

use cargo_test_support::git;
use cargo_test_support::registry::{self, Package};
use cargo_test_support::{basic_lib_manifest, paths, project, Project};

#[cargo_test]
fn vendor_simple() {
    let p = project()
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                log = "0.3.5"
            "#,
        )
        .file("src/lib.rs", "")
        .build();

    Package::new("log", "0.3.5").publish();

    p.cargo("vendor --respect-source-config")
        .with_stdout(
            r#"
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
"#,
        )
        .run();
    let lock = p.read_file("vendor/log/Cargo.toml");
    assert!(lock.contains("version = \"0.3.5\""));

    add_vendor_config(&p);
    p.cargo("build").run();
}

fn add_vendor_config(p: &Project) {
    p.change_file(
        ".cargo/config",
        r#"
            [source.crates-io]
            replace-with = 'vendor'

            [source.vendor]
            directory = 'vendor'
        "#,
    );
}

#[cargo_test]
fn package_exclude() {
    let p = project()
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                bar = "0.1.0"
            "#,
        )
        .file("src/lib.rs", "")
        .build();

    Package::new("bar", "0.1.0")
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "bar"
                version = "0.1.0"
                exclude = [".*", "!.include", "!.dotdir/include"]
            "#,
        )
        .file("src/lib.rs", "")
        .file(".exclude", "")
        .file(".include", "")
        .file(".dotdir/exclude", "")
        .file(".dotdir/include", "")
        .publish();

    p.cargo("vendor --respect-source-config").run();
    let csum = p.read_file("vendor/bar/.cargo-checksum.json");
    assert!(csum.contains(".include"));
    assert!(!csum.contains(".exclude"));
    assert!(!csum.contains(".dotdir/exclude"));
    // Gitignore doesn't re-include a file in an excluded parent directory,
    // even if negating it explicitly.
    assert!(!csum.contains(".dotdir/include"));
}

#[cargo_test]
fn two_versions() {
    let p = project()
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                bitflags = "0.8.0"
                bar = { path = "bar" }
            "#,
        )
        .file("src/lib.rs", "")
        .file(
            "bar/Cargo.toml",
            r#"
                [package]
                name = "bar"
                version = "0.1.0"

                [dependencies]
                bitflags = "0.7.0"
            "#,
        )
        .file("bar/src/lib.rs", "")
        .build();

    Package::new("bitflags", "0.7.0").publish();
    Package::new("bitflags", "0.8.0").publish();

    p.cargo("vendor --respect-source-config").run();

    let lock = p.read_file("vendor/bitflags/Cargo.toml");
    assert!(lock.contains("version = \"0.8.0\""));
    let lock = p.read_file("vendor/bitflags-0.7.0/Cargo.toml");
    assert!(lock.contains("version = \"0.7.0\""));

    add_vendor_config(&p);
    p.cargo("build").run();
}

#[cargo_test]
fn two_explicit_versions() {
    let p = project()
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                bitflags = "0.8.0"
                bar = { path = "bar" }
            "#,
        )
        .file("src/lib.rs", "")
        .file(
            "bar/Cargo.toml",
            r#"
                [package]
                name = "bar"
                version = "0.1.0"

                [dependencies]
                bitflags = "0.7.0"
            "#,
        )
        .file("bar/src/lib.rs", "")
        .build();

    Package::new("bitflags", "0.7.0").publish();
    Package::new("bitflags", "0.8.0").publish();

    p.cargo("vendor --respect-source-config --versioned-dirs")
        .run();

    let lock = p.read_file("vendor/bitflags-0.8.0/Cargo.toml");
    assert!(lock.contains("version = \"0.8.0\""));
    let lock = p.read_file("vendor/bitflags-0.7.0/Cargo.toml");
    assert!(lock.contains("version = \"0.7.0\""));

    add_vendor_config(&p);
    p.cargo("build").run();
}

#[cargo_test]
fn help() {
    let p = project().build();
    p.cargo("vendor -h").run();
}

#[cargo_test]
fn update_versions() {
    let p = project()
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                bitflags = "0.7.0"
            "#,
        )
        .file("src/lib.rs", "")
        .build();

    Package::new("bitflags", "0.7.0").publish();
    Package::new("bitflags", "0.8.0").publish();

    p.cargo("vendor --respect-source-config").run();

    let lock = p.read_file("vendor/bitflags/Cargo.toml");
    assert!(lock.contains("version = \"0.7.0\""));

    p.change_file(
        "Cargo.toml",
        r#"
            [package]
            name = "foo"
            version = "0.1.0"

            [dependencies]
            bitflags = "0.8.0"
        "#,
    );
    p.cargo("vendor --respect-source-config").run();

    let lock = p.read_file("vendor/bitflags/Cargo.toml");
    assert!(lock.contains("version = \"0.8.0\""));
}

#[cargo_test]
fn two_lockfiles() {
    let p = project()
        .no_manifest()
        .file(
            "foo/Cargo.toml",
            r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                bitflags = "=0.7.0"
            "#,
        )
        .file("foo/src/lib.rs", "")
        .file(
            "bar/Cargo.toml",
            r#"
                [package]
                name = "bar"
                version = "0.1.0"

                [dependencies]
                bitflags = "=0.8.0"
            "#,
        )
        .file("bar/src/lib.rs", "")
        .build();

    Package::new("bitflags", "0.7.0").publish();
    Package::new("bitflags", "0.8.0").publish();

    p.cargo("vendor --respect-source-config -s bar/Cargo.toml --manifest-path foo/Cargo.toml")
        .run();

    let lock = p.read_file("vendor/bitflags/Cargo.toml");
    assert!(lock.contains("version = \"0.8.0\""));
    let lock = p.read_file("vendor/bitflags-0.7.0/Cargo.toml");
    assert!(lock.contains("version = \"0.7.0\""));

    add_vendor_config(&p);
    p.cargo("build").cwd("foo").run();
    p.cargo("build").cwd("bar").run();
}

#[cargo_test]
fn delete_old_crates() {
    let p = project()
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                bitflags = "=0.7.0"
            "#,
        )
        .file("src/lib.rs", "")
        .build();

    Package::new("bitflags", "0.7.0").publish();
    Package::new("log", "0.3.5").publish();

    p.cargo("vendor --respect-source-config").run();
    p.read_file("vendor/bitflags/Cargo.toml");

    p.change_file(
        "Cargo.toml",
        r#"
            [package]
            name = "foo"
            version = "0.1.0"

            [dependencies]
            log = "=0.3.5"
        "#,
    );

    p.cargo("vendor --respect-source-config").run();
    let lock = p.read_file("vendor/log/Cargo.toml");
    assert!(lock.contains("version = \"0.3.5\""));
    assert!(!p.root().join("vendor/bitflags/Cargo.toml").exists());
}

#[cargo_test]
fn ignore_files() {
    let p = project()
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                url = "1.4.1"
            "#,
        )
        .file("src/lib.rs", "")
        .build();

    Package::new("url", "1.4.1")
        .file("src/lib.rs", "")
        .file("foo.orig", "")
        .file(".gitignore", "")
        .file(".gitattributes", "")
        .file("foo.rej", "")
        .publish();

    p.cargo("vendor --respect-source-config").run();
    let csum = p.read_file("vendor/url/.cargo-checksum.json");
    assert!(!csum.contains("foo.orig"));
    assert!(!csum.contains(".gitignore"));
    assert!(!csum.contains(".gitattributes"));
    assert!(!csum.contains(".cargo-ok"));
    assert!(!csum.contains("foo.rej"));
}

#[cargo_test]
fn included_files_only() {
    let git = git::new("a", |p| {
        p.file("Cargo.toml", &basic_lib_manifest("a"))
            .file("src/lib.rs", "")
            .file(".gitignore", "a")
            .file("a/b.md", "")
    });

    let p = project()
        .file(
            "Cargo.toml",
            &format!(
                r#"
                    [package]
                    name = "foo"
                    version = "0.1.0"

                    [dependencies]
                    a = {{ git = '{}' }}
                "#,
                git.url()
            ),
        )
        .file("src/lib.rs", "")
        .build();

    p.cargo("vendor --respect-source-config").run();
    let csum = p.read_file("vendor/a/.cargo-checksum.json");
    assert!(!csum.contains("a/b.md"));
}

#[cargo_test]
fn dependent_crates_in_crates() {
    let git = git::new("a", |p| {
        p.file(
            "Cargo.toml",
            r#"
                [package]
                name = "a"
                version = "0.1.0"

                [dependencies]
                b = { path = 'b' }
            "#,
        )
        .file("src/lib.rs", "")
        .file("b/Cargo.toml", &basic_lib_manifest("b"))
        .file("b/src/lib.rs", "")
    });
    let p = project()
        .file(
            "Cargo.toml",
            &format!(
                r#"
                    [package]
                    name = "foo"
                    version = "0.1.0"

                    [dependencies]
                    a = {{ git = '{}' }}
                "#,
                git.url()
            ),
        )
        .file("src/lib.rs", "")
        .build();

    p.cargo("vendor --respect-source-config").run();
    p.read_file("vendor/a/.cargo-checksum.json");
    p.read_file("vendor/b/.cargo-checksum.json");
}

#[cargo_test]
fn vendoring_git_crates() {
    let git = git::new("git", |p| {
        p.file("Cargo.toml", &basic_lib_manifest("serde_derive"))
            .file("src/lib.rs", "")
            .file("src/wut.rs", "")
    });

    let p = project()
        .file(
            "Cargo.toml",
            &format!(
                r#"
                    [package]
                    name = "foo"
                    version = "0.1.0"

                    [dependencies.serde]
                    version = "0.5.0"

                    [dependencies.serde_derive]
                    version = "0.5.0"

                    [patch.crates-io]
                    serde_derive = {{ git = '{}' }}
                "#,
                git.url()
            ),
        )
        .file("src/lib.rs", "")
        .build();
    Package::new("serde", "0.5.0")
        .dep("serde_derive", "0.5")
        .publish();
    Package::new("serde_derive", "0.5.0").publish();

    p.cargo("vendor --respect-source-config").run();
    p.read_file("vendor/serde_derive/src/wut.rs");

    add_vendor_config(&p);
    p.cargo("build").run();
}

#[cargo_test]
fn git_simple() {
    let git = git::new("git", |p| {
        p.file("Cargo.toml", &basic_lib_manifest("a"))
            .file("src/lib.rs", "")
    });

    let p = project()
        .file(
            "Cargo.toml",
            &format!(
                r#"
                    [package]
                    name = "foo"
                    version = "0.1.0"

                    [dependencies]
                    a = {{ git = '{}' }}
                "#,
                git.url()
            ),
        )
        .file("src/lib.rs", "")
        .build();

    p.cargo("vendor --respect-source-config").run();
    let csum = p.read_file("vendor/a/.cargo-checksum.json");
    assert!(csum.contains("\"package\":null"));
}

#[cargo_test]
fn duplicate_version_from_multiple_sources() {
    registry::alt_init();
    Package::new("b", "0.5.0").publish();
    Package::new("bar", "0.1.0").publish();

    // Start with merged source (no duplicate versions).
    let p = project()
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                bar = '0.1.0'
                b = '0.5.0'

            "#,
        )
        .file("src/lib.rs", "")
        .build();

    p.cargo("vendor --respect-source-config").run();

    let assert_top_level_vendor_crates_exists = || {
        let manifest = p.read_file("vendor/b/Cargo.toml");
        assert!(manifest.contains(r#"version = "0.5.0""#));
        let manifest = p.read_file("vendor/bar/Cargo.toml");
        assert!(manifest.contains(r#"version = "0.1.0""#));
    };
    assert_top_level_vendor_crates_exists();

    add_vendor_config(&p);
    p.cargo("check -v").run();

    // Add conflict versions
    Package::new("b", "0.5.0").alternative(true).publish();
    let git_dep = git::new("b", |p| {
        p.file("Cargo.toml", &basic_lib_manifest("b"))
            .file("src/lib.rs", "")
    });

    // Switch to duplicate versions.
    p.change_file(
        "Cargo.toml",
        &format!(
            r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                bar = '0.1.0'
                b = '0.5.0'
                git_b = {{ git = '{}', package = "b" }}
                alt_b = {{ version = "0.5.0", registry = "alternative", package = "b" }}
            "#,
            git_dep.url(),
        ),
    );

    // Remove previous added source configs.
    p.change_file(".cargo/config", "");
    p.cargo("clean").run();

    // Sources separate due to duplicate versions.
    let output = p
        .cargo("vendor --respect-source-config")
        .exec_with_output()
        .unwrap();
    // The vendored crates in the top level won't be delete.
    assert_top_level_vendor_crates_exists();

    let output = String::from_utf8(output.stdout).unwrap();
    p.change_file(".cargo/config", &output);

    p.cargo("check -v")
        .with_stderr_contains("[..]foo/vendor/b/src/lib.rs[..]")
        .with_stderr_contains("[..]foo/vendor/bar/src/lib.rs[..]")
        .with_stderr_contains("[..]foo/vendor/@git-[..]/b/src/lib.rs[..]")
        .with_stderr_contains("[..]foo/vendor/@registry-[..]/b/src/lib.rs[..]")
        .run();

    // Switch to duplicate-free versions.
    // Source should be merged again.
    Package::new("b", "0.4.0").alternative(true).publish();
    let git_dep = git::new("b-0.3.0", |p| {
        p.file(
            "Cargo.toml",
            r#"
                [package]
                name = "b"
                version = "0.3.0"
            "#,
        )
        .file("src/lib.rs", "")
    });
    p.change_file(
        "Cargo.toml",
        &format!(
            r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                bar = '0.1.0'
                b = '0.5.0'
                git_b = {{ git = '{}', package = "b" }}
                alt_b = {{ version = "0.4.0", registry = "alternative", package = "b" }}
            "#,
            git_dep.url(),
        ),
    );

    // Remove previous added source configs.
    p.change_file(".cargo/config", "");
    p.cargo("clean").run();

    let output = p
        .cargo("vendor --respect-source-config")
        .exec_with_output()
        .unwrap();
    p.cargo("vendor --respect-source-config").run();
    // The vendored crates in the top level won't be delete.
    assert_top_level_vendor_crates_exists();
    let manifest = p.read_file("vendor/b-0.4.0/Cargo.toml");
    assert!(manifest.contains(r#"version = "0.4.0""#));
    let manifest = p.read_file("vendor/b-0.3.0/Cargo.toml");
    assert!(manifest.contains(r#"version = "0.3.0""#));

    let output = String::from_utf8(output.stdout).unwrap();
    p.change_file(".cargo/config", &output);
    p.cargo("check -v")
        .with_stderr_contains("[..]foo/vendor/bar/src/lib.rs[..]")
        .with_stderr_contains("[..]foo/vendor/b/src/lib.rs[..]")
        .with_stderr_contains("[..]foo/vendor/b-0.4.0/src/lib.rs[..]")
        .with_stderr_contains("[..]foo/vendor/b-0.3.0/src/lib.rs[..]")
        .run();
    for entry in p.root().join("vendor").read_dir().unwrap() {
        let entry = entry.unwrap();
        assert!(
            !entry.file_name().into_string().unwrap().starts_with('@'),
            "unused source should be deleted: {}",
            entry.path().display(),
        );
    }
}

#[cargo_test]
fn depend_on_vendor_dir_not_deleted() {
    let p = project()
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                libc = "0.2.30"
            "#,
        )
        .file("src/lib.rs", "")
        .build();

    Package::new("libc", "0.2.30").publish();

    p.cargo("vendor --respect-source-config").run();
    assert!(p.root().join("vendor/libc").is_dir());

    p.change_file(
        "Cargo.toml",
        r#"
            [package]
            name = "foo"
            version = "0.1.0"

            [dependencies]
            libc = "0.2.30"

            [patch.crates-io]
            libc = { path = 'vendor/libc' }
        "#,
    );

    p.cargo("vendor --respect-source-config").run();
    assert!(p.root().join("vendor/libc").is_dir());
}

#[cargo_test]
fn ignore_hidden() {
    // Don't delete files starting with `.`
    Package::new("bar", "0.1.0").publish();
    let p = project()
        .file(
            "Cargo.toml",
            r#"
            [package]
            name = "foo"
            version = "1.0.0"
            [dependencies]
            bar = "0.1.0"
            "#,
        )
        .file("src/lib.rs", "")
        .build();
    p.cargo("vendor --respect-source-config").run();
    // Add a `.git` directory.
    let repo = git::init(&p.root().join("vendor"));
    git::add(&repo);
    git::commit(&repo);
    assert!(p.root().join("vendor/.git").exists());
    // Vendor again, shouldn't change anything.
    p.cargo("vendor --respect-source-config").run();
    // .git should not be removed.
    assert!(p.root().join("vendor/.git").exists());
    // And just for good measure, make sure no files changed.
    let mut opts = git2::StatusOptions::new();
    assert!(repo
        .statuses(Some(&mut opts))
        .unwrap()
        .iter()
        .all(|status| status.status() == git2::Status::CURRENT));
}

#[cargo_test]
fn config_instructions_works() {
    // Check that the config instructions work for all dependency kinds.
    registry::alt_init();
    Package::new("dep", "0.1.0").publish();
    Package::new("altdep", "0.1.0").alternative(true).publish();
    let git_project = git::new("gitdep", |project| {
        project
            .file("Cargo.toml", &basic_lib_manifest("gitdep"))
            .file("src/lib.rs", "")
    });
    let p = project()
        .file(
            "Cargo.toml",
            &format!(
                r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                dep = "0.1"
                altdep = {{version="0.1", registry="alternative"}}
                gitdep = {{git='{}'}}
                "#,
                git_project.url()
            ),
        )
        .file("src/lib.rs", "")
        .build();
    let output = p
        .cargo("vendor --respect-source-config")
        .exec_with_output()
        .unwrap();
    let output = String::from_utf8(output.stdout).unwrap();
    p.change_file(".cargo/config", &output);

    p.cargo("check -v")
        .with_stderr_contains("[..]foo/vendor/dep/src/lib.rs[..]")
        .with_stderr_contains("[..]foo/vendor/altdep/src/lib.rs[..]")
        .with_stderr_contains("[..]foo/vendor/gitdep/src/lib.rs[..]")
        .run();
}

#[cargo_test]
fn git_crlf_preservation() {
    // Check that newlines don't get changed when you vendor
    // (will only fail if your system is setup with core.autocrlf=true on windows)
    let input = "hello \nthere\nmy newline\nfriends";
    let git_project = git::new("git", |p| {
        p.file("Cargo.toml", &basic_lib_manifest("a"))
            .file("src/lib.rs", input)
    });

    let p = project()
        .file(
            "Cargo.toml",
            &format!(
                r#"
                    [package]
                    name = "foo"
                    version = "0.1.0"

                    [dependencies]
                    a = {{ git = '{}' }}
                "#,
                git_project.url()
            ),
        )
        .file("src/lib.rs", "")
        .build();

    fs::write(
        paths::home().join(".gitconfig"),
        r#"
            [core]
            autocrlf = true
        "#,
    )
    .unwrap();

    p.cargo("vendor --respect-source-config").run();
    let output = p.read_file("vendor/a/src/lib.rs");
    assert_eq!(input, output);
}

#[cargo_test]
#[cfg(unix)]
fn vendor_preserves_permissions() {
    use std::os::unix::fs::MetadataExt;

    Package::new("bar", "1.0.0")
        .file_with_mode("example.sh", 0o755, "#!/bin/sh")
        .file("src/lib.rs", "")
        .publish();

    let p = project()
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "foo"
                version = "0.1.0"

                [dependencies]
                bar = "1.0"
            "#,
        )
        .file("src/lib.rs", "")
        .build();

    p.cargo("vendor --respect-source-config").run();

    let metadata = fs::metadata(p.root().join("vendor/bar/src/lib.rs")).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o644);
    let metadata = fs::metadata(p.root().join("vendor/bar/example.sh")).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o755);
}

#[cargo_test]
fn no_remote_dependency_no_vendor() {
    let p = project()
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "foo"
                version = "0.1.0"
                [dependencies]
                bar = { path = "bar" }
            "#,
        )
        .file("src/lib.rs", "")
        .file(
            "bar/Cargo.toml",
            r#"
                [package]
                name = "bar"
                version = "0.1.0"
            "#,
        )
        .file("bar/src/lib.rs", "")
        .build();

    p.cargo("vendor")
        .with_stderr("There is no dependency to vendor in this project.")
        .run();
    assert!(!p.root().join("vendor").exists());
}
