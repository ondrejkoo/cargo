use std::cell::RefCell;
use std::collections::hash_map::{Entry, HashMap};
use std::path::PathBuf;
use std::str::{self, FromStr};

use super::env_args;
use super::Kind;
use crate::core::TargetKind;
use crate::util::{CargoResult, CargoResultExt, Cfg, Config, ProcessBuilder, Rustc};

#[derive(Clone)]
pub struct TargetInfo {
    crate_type_process: Option<ProcessBuilder>,
    crate_types: RefCell<HashMap<String, Option<(String, String)>>>,
    cfg: Option<Vec<Cfg>>,
    pub sysroot_libdir: Option<PathBuf>,
}

/// Type of each file generated by a Unit.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FileFlavor {
    /// Not a special file type.
    Normal,
    /// Something you can link against (e.g., a library).
    Linkable,
    /// Piece of external debug information (e.g., `.dSYM`/`.pdb` file).
    DebugInfo,
}

pub struct FileType {
    pub flavor: FileFlavor,
    suffix: String,
    prefix: String,
    // Wasm bin target will generate two files in deps such as
    // "web-stuff.js" and "web_stuff.wasm". Note the different usages of
    // "-" and "_". should_replace_hyphens is a flag to indicate that
    // we need to convert the stem "web-stuff" to "web_stuff", so we
    // won't miss "web_stuff.wasm".
    should_replace_hyphens: bool,
}

impl FileType {
    pub fn filename(&self, stem: &str) -> String {
        let stem = if self.should_replace_hyphens {
            stem.replace("-", "_")
        } else {
            stem.to_string()
        };
        format!("{}{}{}", self.prefix, stem, self.suffix)
    }
}

impl TargetInfo {
    pub fn new(
        config: &Config,
        requested_target: &Option<String>,
        rustc: &Rustc,
        kind: Kind,
    ) -> CargoResult<TargetInfo> {
        let rustflags = env_args(
            config,
            requested_target,
            &rustc.host,
            None,
            kind,
            "RUSTFLAGS",
        )?;
        let mut process = rustc.process();
        process
            .arg("-")
            .arg("--crate-name")
            .arg("___")
            .arg("--print=file-names")
            .args(&rustflags)
            .env_remove("RUST_LOG");

        let target_triple = requested_target
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(&rustc.host);
        if kind == Kind::Target {
            process.arg("--target").arg(target_triple);
        }

        let crate_type_process = process.clone();
        const KNOWN_CRATE_TYPES: &[&str] =
            &["bin", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"];
        for crate_type in KNOWN_CRATE_TYPES.iter() {
            process.arg("--crate-type").arg(crate_type);
        }

        let mut with_cfg = process.clone();
        with_cfg.arg("--print=sysroot");
        with_cfg.arg("--print=cfg");

        let mut has_cfg_and_sysroot = true;
        let (output, error) = rustc
            .cached_output(&with_cfg)
            .or_else(|_| {
                has_cfg_and_sysroot = false;
                rustc.cached_output(&process)
            })
            .chain_err(|| "failed to run `rustc` to learn about target-specific information")?;

        let mut lines = output.lines();
        let mut map = HashMap::new();
        for crate_type in KNOWN_CRATE_TYPES {
            let out = parse_crate_type(crate_type, &error, &mut lines)?;
            map.insert(crate_type.to_string(), out);
        }

        let mut sysroot_libdir = None;
        if has_cfg_and_sysroot {
            let line = match lines.next() {
                Some(line) => line,
                None => failure::bail!(
                    "output of --print=sysroot missing when learning about \
                     target-specific information from rustc"
                ),
            };
            let mut rustlib = PathBuf::from(line);
            if kind == Kind::Host {
                if cfg!(windows) {
                    rustlib.push("bin");
                } else {
                    rustlib.push("lib");
                }
                sysroot_libdir = Some(rustlib);
            } else {
                rustlib.push("lib");
                rustlib.push("rustlib");
                rustlib.push(target_triple);
                rustlib.push("lib");
                sysroot_libdir = Some(rustlib);
            }
        }

        let cfg = if has_cfg_and_sysroot {
            Some(lines.map(Cfg::from_str).collect::<CargoResult<_>>()?)
        } else {
            None
        };

        Ok(TargetInfo {
            crate_type_process: Some(crate_type_process),
            crate_types: RefCell::new(map),
            cfg,
            sysroot_libdir,
        })
    }

    pub fn cfg(&self) -> Option<&[Cfg]> {
        self.cfg.as_ref().map(|v| v.as_ref())
    }

    pub fn file_types(
        &self,
        crate_type: &str,
        flavor: FileFlavor,
        kind: &TargetKind,
        target_triple: &str,
    ) -> CargoResult<Option<Vec<FileType>>> {
        let mut crate_types = self.crate_types.borrow_mut();
        let entry = crate_types.entry(crate_type.to_string());
        let crate_type_info = match entry {
            Entry::Occupied(o) => &*o.into_mut(),
            Entry::Vacant(v) => {
                let value = self.discover_crate_type(v.key())?;
                &*v.insert(value)
            }
        };
        let (prefix, suffix) = match *crate_type_info {
            Some((ref prefix, ref suffix)) => (prefix, suffix),
            None => return Ok(None),
        };
        let mut ret = vec![FileType {
            suffix: suffix.clone(),
            prefix: prefix.clone(),
            flavor,
            should_replace_hyphens: false,
        }];

        // See rust-lang/cargo#4500.
        if target_triple.ends_with("pc-windows-msvc")
            && crate_type.ends_with("dylib")
            && suffix == ".dll"
        {
            ret.push(FileType {
                suffix: ".dll.lib".to_string(),
                prefix: prefix.clone(),
                flavor: FileFlavor::Normal,
                should_replace_hyphens: false,
            })
        }

        // See rust-lang/cargo#4535.
        if target_triple.starts_with("wasm32-") && crate_type == "bin" && suffix == ".js" {
            ret.push(FileType {
                suffix: ".wasm".to_string(),
                prefix: prefix.clone(),
                flavor: FileFlavor::Normal,
                should_replace_hyphens: true,
            })
        }

        // See rust-lang/cargo#4490, rust-lang/cargo#4960.
        //  - Only uplift debuginfo for binaries.
        //    Tests are run directly from `target/debug/deps/`
        //    and examples are inside target/debug/examples/ which already have symbols next to them,
        //    so no need to do anything.
        if *kind == TargetKind::Bin {
            if target_triple.contains("-apple-") {
                ret.push(FileType {
                    suffix: ".dSYM".to_string(),
                    prefix: prefix.clone(),
                    flavor: FileFlavor::DebugInfo,
                    should_replace_hyphens: false,
                })
            } else if target_triple.ends_with("-msvc") {
                ret.push(FileType {
                    suffix: ".pdb".to_string(),
                    prefix: prefix.clone(),
                    flavor: FileFlavor::DebugInfo,
                    should_replace_hyphens: false,
                })
            }
        }

        Ok(Some(ret))
    }

    fn discover_crate_type(&self, crate_type: &str) -> CargoResult<Option<(String, String)>> {
        let mut process = self.crate_type_process.clone().unwrap();

        process.arg("--crate-type").arg(crate_type);

        let output = process.exec_with_output().chain_err(|| {
            format!(
                "failed to run `rustc` to learn about \
                 crate-type {} information",
                crate_type
            )
        })?;

        let error = str::from_utf8(&output.stderr).unwrap();
        let output = str::from_utf8(&output.stdout).unwrap();
        Ok(parse_crate_type(crate_type, error, &mut output.lines())?)
    }
}

/// Takes rustc output (using specialized command line args), and calculates the file prefix and
/// suffix for the given crate type, or returns `None` if the type is not supported. (e.g., for a
/// Rust library like `libcargo.rlib`, we have prefix "lib" and suffix "rlib").
///
/// The caller needs to ensure that the lines object is at the correct line for the given crate
/// type: this is not checked.
//
// This function can not handle more than one file per type (with wasm32-unknown-emscripten, there
// are two files for bin (`.wasm` and `.js`)).
fn parse_crate_type(
    crate_type: &str,
    error: &str,
    lines: &mut str::Lines<'_>,
) -> CargoResult<Option<(String, String)>> {
    let not_supported = error.lines().any(|line| {
        (line.contains("unsupported crate type") || line.contains("unknown crate type"))
            && line.contains(crate_type)
    });
    if not_supported {
        return Ok(None);
    }
    let line = match lines.next() {
        Some(line) => line,
        None => failure::bail!(
            "malformed output when learning about \
             crate-type {} information",
            crate_type
        ),
    };
    let mut parts = line.trim().split("___");
    let prefix = parts.next().unwrap();
    let suffix = match parts.next() {
        Some(part) => part,
        None => failure::bail!(
            "output of --print=file-names has changed in \
             the compiler, cannot parse"
        ),
    };

    Ok(Some((prefix.to_string(), suffix.to_string())))
}
