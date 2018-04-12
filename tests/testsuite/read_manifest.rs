use cargotest::support::{basic_bin_manifest, execs, main_file, project};
use hamcrest::assert_that;

static MANIFEST_OUTPUT: &'static str = r#"
{
    "authors": [
        "wycats@example.com"
    ],
    "categories": [],
    "name":"foo",
    "readme": null,
    "repository": null,
    "version":"0.5.0",
    "id":"foo[..]0.5.0[..](path+file://[..]/foo)",
    "keywords": [],
    "license": null,
    "license_file": null,
    "description": null,
    "source":null,
    "dependencies":[],
    "targets":[{
        "kind":["bin"],
        "crate_types":["bin"],
        "name":"foo",
        "platform": null,
        "src_path":"[..][/]foo[/]src[/]foo.rs"
    }],
    "features":{},
    "manifest_path":"[..]Cargo.toml",
    "metadata": null
}"#;

#[test]
fn cargo_read_manifest_path_to_cargo_toml_relative() {
    let p = project("foo")
        .file("Cargo.toml", &basic_bin_manifest("foo"))
        .file("src/foo.rs", &main_file(r#""i am foo""#, &[]))
        .build();

    assert_that(
        p.cargo("read-manifest")
            .arg("--manifest-path")
            .arg("foo/Cargo.toml")
            .cwd(p.root().parent().unwrap()),
        execs().with_status(0).with_json(MANIFEST_OUTPUT),
    );
}

#[test]
fn cargo_read_manifest_path_to_cargo_toml_absolute() {
    let p = project("foo")
        .file("Cargo.toml", &basic_bin_manifest("foo"))
        .file("src/foo.rs", &main_file(r#""i am foo""#, &[]))
        .build();

    assert_that(
        p.cargo("read-manifest")
            .arg("--manifest-path")
            .arg(p.root().join("Cargo.toml"))
            .cwd(p.root().parent().unwrap()),
        execs().with_status(0).with_json(MANIFEST_OUTPUT),
    );
}

#[test]
fn cargo_read_manifest_path_to_cargo_toml_parent_relative() {
    let p = project("foo")
        .file("Cargo.toml", &basic_bin_manifest("foo"))
        .file("src/foo.rs", &main_file(r#""i am foo""#, &[]))
        .build();

    assert_that(
        p.cargo("read-manifest")
            .arg("--manifest-path")
            .arg("foo")
            .cwd(p.root().parent().unwrap()),
        execs().with_status(101).with_stderr(
            "[ERROR] the manifest-path must be \
             a path to a Cargo.toml file",
        ),
    );
}

#[test]
fn cargo_read_manifest_path_to_cargo_toml_parent_absolute() {
    let p = project("foo")
        .file("Cargo.toml", &basic_bin_manifest("foo"))
        .file("src/foo.rs", &main_file(r#""i am foo""#, &[]))
        .build();

    assert_that(
        p.cargo("read-manifest")
            .arg("--manifest-path")
            .arg(p.root())
            .cwd(p.root().parent().unwrap()),
        execs().with_status(101).with_stderr(
            "[ERROR] the manifest-path must be \
             a path to a Cargo.toml file",
        ),
    );
}

#[test]
fn cargo_read_manifest_cwd() {
    let p = project("foo")
        .file("Cargo.toml", &basic_bin_manifest("foo"))
        .file("src/foo.rs", &main_file(r#""i am foo""#, &[]))
        .build();

    assert_that(
        p.cargo("read-manifest").cwd(p.root()),
        execs().with_status(0).with_json(MANIFEST_OUTPUT),
    );
}
