//! Network tests for https transport.
//!
//! Note that these tests will generally require setting CARGO_CONTAINER_TESTS
//! or CARGO_PUBLIC_NETWORK_TESTS.

use cargo_test_support::containers::Container;
use cargo_test_support::paths::{self, CargoPathExt};
use cargo_test_support::project;

#[cargo_test(container_test)]
fn self_signed_should_fail() {
    // Cargo should not allow a connection to a self-signed certificate.
    let apache = Container::new("apache").launch();
    let port = apache.port_mappings[&443];
    let url = format!("https://127.0.0.1:{port}/repos/bar.git");
    let p = project()
        .file(
            "Cargo.toml",
            &format!(
                r#"
                    [package]
                    name = "foo"
                    version = "0.1.0"
                    edition = "2015"

                    [dependencies]
                    bar = {{ git = "{url}" }}
                "#
            ),
        )
        .file("src/lib.rs", "")
        .build();
    // I think the text here depends on the curl backend.
    let err_msg = if cfg!(target_os = "macos") {
        "untrusted connection error; class=Ssl (16); code=Certificate (-17)"
    } else if cfg!(unix) {
        "the SSL certificate is invalid; class=Ssl (16); code=Certificate (-17)"
    } else if cfg!(windows) {
        "user cancelled certificate check; class=Http (34); code=Certificate (-17)"
    } else {
        panic!("target not supported");
    };
    p.cargo("fetch")
        .with_status(101)
        .with_stderr(&format!(
            "\
[UPDATING] git repository `https://127.0.0.1:[..]/repos/bar.git`
error: failed to get `bar` as a dependency of package `foo v0.1.0 ([ROOT]/foo)`

Caused by:
  failed to load source for dependency `bar`

Caused by:
  Unable to update https://127.0.0.1:[..]/repos/bar.git

Caused by:
  failed to clone into: [ROOT]/home/.cargo/git/db/bar-[..]

Caused by:
  network failure seems to have happened
  if a proxy or similar is necessary `net.git-fetch-with-cli` may help here
  https://doc.rust-lang.org/cargo/reference/config.html#netgit-fetch-with-cli

Caused by:
  {err_msg}
"
        ))
        .run();
}

#[cargo_test(container_test)]
fn self_signed_with_cacert() {
    // When using cainfo, that should allow a connection to a self-signed cert.

    if cfg!(target_os = "macos") {
        // This test only seems to work with the
        // curl-sys/force-system-lib-on-osx feature enabled. For some reason
        // SecureTransport doesn't seem to like the self-signed certificate.
        // It works if the certificate is manually approved via Keychain
        // Access. The system libcurl is built with a LibreSSL fallback which
        // is used when CAINFO is set, which seems to work correctly. This
        // could use some more investigation. The official Rust binaries use
        // curl-sys/force-system-lib-on-osx so it is mostly an issue for local
        // testing.
        //
        // The error is:
        // [60] SSL peer certificate or SSH remote key was not OK (SSL:
        // certificate verification failed (result: 5)); class=Net (12)
        let curl_v = curl::Version::get();
        if curl_v.vendored() {
            eprintln!(
                "vendored curl not supported on macOS, \
                set curl-sys/force-system-lib-on-osx to enable"
            );
            return;
        }
    }

    let apache = Container::new("apache").launch();
    let port = apache.port_mappings[&443];
    let url = format!("https://127.0.0.1:{port}/repos/bar.git");
    let server_crt = apache.read_file("/usr/local/apache2/conf/server.crt");
    let p = project()
        .file(
            "Cargo.toml",
            &format!(
                r#"
                    [package]
                    name = "foo"
                    version = "0.1.0"
                    edition = "2015"

                    [dependencies]
                    bar = {{ git = "{url}" }}
                "#
            ),
        )
        .file("src/lib.rs", "")
        .file(
            ".cargo/config.toml",
            &format!(
                r#"
                    [http]
                    cainfo = "server.crt"
                "#
            ),
        )
        .file("server.crt", &server_crt)
        .build();
    p.cargo("fetch")
        .with_stderr("[UPDATING] git repository `https://127.0.0.1:[..]/repos/bar.git`")
        .run();
}

#[cargo_test(public_network_test)]
fn github_works() {
    // Check that an https connection to github.com works.
    // This tries all the different types of git references, and verifies the fast-path behavior.
    for (manifest_ref, oid, refspecs, up_to_date) in [
        (
            r#", tag = "1.3.2""#,
            "ed185cfb1c447c1b4bd6ac021c9ec3bb02c9e2f2",
            r#""+refs/tags/1.3.2:refs/remotes/origin/tags/1.3.2""#,
            "github fast path up-to-date",
        ),
        (
            r#", rev = "6c67922300d5abae779ca147bac00f6ff9c87f8a""#,
            "6c67922300d5abae779ca147bac00f6ff9c87f8a",
            r#""+6c67922300d5abae779ca147bac00f6ff9c87f8a:refs/commit/6c67922300d5abae779ca147bac00f6ff9c87f8a""#,
            "github fast path already has 6c67922300d5abae779ca147bac00f6ff9c87f8a",
        ),
        (
            r#", branch = "main""#,
            "[..]",
            r#""+refs/heads/main:refs/remotes/origin/main""#,
            "github fast path up-to-date",
        ),
        (
            "",
            "[..]",
            r#""+HEAD:refs/remotes/origin/HEAD""#,
            "github fast path up-to-date",
        ),
    ] {
        eprintln!("test {manifest_ref}");
        let p = project()
            .file(
                "Cargo.toml",
                &format!(
                    r#"
                    [package]
                    name = "foo"
                    version = "0.1.0"
                    edition = "2015"

                    [dependencies]
                    bitflags = {{ git = "https://github.com/rust-lang/bitflags.git"{manifest_ref}}}
                "#
                ),
            )
            .file("src/lib.rs", "")
            .build();
        p.cargo("fetch")
            .env("CARGO_LOG", "cargo::sources::git::utils=debug")
            .with_stderr_contains("[UPDATING] git repository `https://github.com/rust-lang/bitflags.git`")
            .with_stderr_contains("[..]attempting GitHub fast path[..]")
            .with_stderr_contains(&format!("[..]github fast path fetch {oid}"))
            .with_stderr_contains(&format!("[..]initiating fetch of [{refspecs}] from https://github.com/rust-lang/bitflags.git"))
            .run();
        // Remove the lock file, and test the up-to-date code path.
        p.root().join("Cargo.lock").rm_rf();
        p.cargo("fetch")
            .env("CARGO_LOG", "cargo::sources::git::utils=debug")
            .with_stderr_contains(&format!("[..]{up_to_date}"))
            .run();

        paths::home().join(".cargo/git").rm_rf();
    }
}
