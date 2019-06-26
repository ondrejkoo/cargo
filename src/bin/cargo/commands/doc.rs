use crate::command_prelude::*;

use cargo::ops::{self, DocOptions};

pub fn cli() -> App {
    subcommand("doc")
        .about("Build a package's documentation")
        .arg(opt("quiet", "No output printed to stdout").short("q"))
        .arg(opt(
            "open",
            "Opens the docs in a browser after the operation",
        ))
        .arg_package_spec(
            "Package to document",
            "Document all packages in the workspace",
            "Exclude packages from the build",
        )
        .arg(opt("no-deps", "Don't build documentation for dependencies"))
        .arg(opt("document-private-items", "Document private items"))
        .arg_jobs()
        .arg_targets_lib_bin(
            "Document only this package's library",
            "Document only the specified binary",
            "Document all binaries",
        )
        .arg_release("Build artifacts in release mode, with optimizations")
        .arg_features()
        .arg_target_triple("Build for the target triple")
        .arg_target_dir()
        .arg(
            Arg::with_name("use-local-sysroot-docs")
                .long("use-local-sysroot-docs")
                .help("Attempt to use local sysroot file:// links if available")
                .hidden(true),
        )
        .arg_manifest_path()
        .arg_message_format()
        .after_help(
            "\
By default the documentation for the local package and all dependencies is
built. The output is all placed in `target/doc` in rustdoc's usual format.

All packages in the workspace are documented if the `--all` flag is supplied. The
`--all` flag is automatically assumed for a virtual manifest.
Note that `--exclude` has to be specified in conjunction with the `--all` flag.

If the `--package` argument is given, then SPEC is a package ID specification
which indicates which package should be documented. If it is not given, then the
current package is documented. For more information on SPEC and its format, see
the `cargo help pkgid` command.
",
        )
}

pub fn exec(config: &mut Config, args: &ArgMatches<'_>) -> CliResult {
    let ws = args.workspace(config)?;

    let mode = CompileMode::Doc {
        deps: !args.is_present("no-deps"),
    };

    let mut compile_opts = args.compile_options(config, mode, Some(&ws))?;
    compile_opts.local_rustdoc_args = if args.is_present("document-private-items") {
        Some(vec!["--document-private-items".to_string()])
    } else {
        None
    };

    let use_local_sysroot_docs = args.is_present("use-local-sysroot-docs");
    if use_local_sysroot_docs && !config.cli_unstable().unstable_options {
        return Err(failure::format_err!(
            "`cargo doc --use-local-sysroot-docs` is unstable, pass `-Z unstable-options` to enable it"
        )
        .into());
    }

    compile_opts.build_config.use_local_sysroot_docs = use_local_sysroot_docs;

    let doc_opts = DocOptions {
        open_result: args.is_present("open"),
        compile_opts,
    };

    ops::doc(&ws, &doc_opts)?;

    Ok(())
}
