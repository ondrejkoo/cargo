use crate::core::{Shell, Workspace};
use crate::ops;
use crate::util::config::{Config, PathAndArgs};
use crate::util::CargoResult;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

/// Strongly typed options for the `cargo doc` command.
#[derive(Debug)]
pub struct DocOptions {
    /// Whether to attempt to open the browser after compiling the docs
    pub open_result: bool,
    /// Options to pass through to the compiler
    pub compile_opts: ops::CompileOptions,
}

/// Main method for `cargo doc`.
pub fn doc(ws: &Workspace<'_>, options: &DocOptions) -> CargoResult<()> {
    let compilation = ops::compile(ws, &options.compile_opts)?;

    if options.open_result {
        // The open behavior is as follows:
        // cargo doc --open:
        //  - Pick the first root unit that was built for host.
        //  - If none found, pick the first one(whatever it's target is).
        // cargo doc --target TARGET --open:
        //  - Pick the first root unit for the given target.
        //  - If none found, pick the first one(whatever it's target is).
        let request_kind = options.compile_opts.build_config.single_requested_kind()?;
        let (name, kind) = &compilation
            .root_crate_names
            .iter()
            .find(|(_, kind)| *kind == request_kind)
            .or_else(|| compilation.root_crate_names.get(0))
            .ok_or_else(|| anyhow::anyhow!("no crates with documentation"))?;

        let path = compilation.root_output[&kind]
            .with_file_name("doc")
            .join(&name)
            .join("index.html");
        if path.exists() {
            let config_browser = {
                let cfg: Option<PathAndArgs> = ws.config().get("doc.browser")?;
                cfg.map(|path_args| (path_args.path.resolve_program(ws.config()), path_args.args))
            };
            let mut shell = ws.config().shell();
            let link = shell.err_file_hyperlink(&path);
            shell.status(
                "Opening",
                format!("{}{}{}", link.open(), path.display(), link.close()),
            )?;
            open_docs(&path, &mut shell, config_browser, ws.config())?;
        }
    } else {
        for name in &compilation.root_crate_names {
            for kind in &options.compile_opts.build_config.requested_kinds {
                let path = compilation.root_output[&kind]
                    .with_file_name("doc")
                    .join(&name)
                    .join("index.html");
                if path.exists() {
                    let mut shell = ws.config().shell();
                    let link = shell.err_file_hyperlink(&path);
                    shell.status(
                        "Generated",
                        format!("{}{}{}", link.open(), path.display(), link.close()),
                    )?;
                }
            }
        }
    }

    Ok(())
}

fn open_docs(
    path: &Path,
    shell: &mut Shell,
    config_browser: Option<(PathBuf, Vec<String>)>,
    config: &Config,
) -> CargoResult<()> {
    let browser =
        config_browser.or_else(|| Some((PathBuf::from(config.get_env_os("BROWSER")?), Vec::new())));

    match browser {
        Some((browser, initial_args)) => {
            if let Err(e) = Command::new(&browser).args(initial_args).arg(path).status() {
                shell.warn(format!(
                    "Couldn't open docs with {}: {}",
                    browser.to_string_lossy(),
                    e
                ))?;
            }
        }
        None => {
            if let Err(e) = opener::open(&path) {
                let e = e.into();
                crate::display_warning_with_error("couldn't open docs", &e, shell);
            }
        }
    };

    Ok(())
}
