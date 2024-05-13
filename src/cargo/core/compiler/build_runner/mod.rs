//! [`BuildRunner`] is the mutable state used during the build process.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::core::compiler::compilation::{self, UnitOutput};
use crate::core::compiler::{self, artifact, Unit};
use crate::core::PackageId;
use crate::util::cache_lock::CacheLockMode;
use crate::util::errors::CargoResult;
use anyhow::{bail, Context as _};
use filetime::FileTime;
use itertools::Itertools;
use jobserver::Client;

use super::build_plan::BuildPlan;
use super::custom_build::{self, BuildDeps, BuildScriptOutputs, BuildScripts};
use super::fingerprint::Fingerprint;
use super::job_queue::JobQueue;
use super::layout::Layout;
use super::lto::Lto;
use super::unit_graph::UnitDep;
use super::{
    BuildContext, Compilation, CompileKind, CompileMode, Executor, FileFlavor, RustDocFingerprint,
};

mod compilation_files;
use self::compilation_files::CompilationFiles;
pub use self::compilation_files::{Metadata, OutputFile};

/// Collection of all the stuff that is needed to perform a build.
///
/// Different from the [`BuildContext`], `Context` is a _mutable_ state used
/// throughout the entire build process. Everything is coordinated through this.
///
/// [`BuildContext`]: crate::core::compiler::BuildContext
pub struct BuildRunner<'a, 'gctx> {
    /// Mostly static information about the build task.
    pub bcx: &'a BuildContext<'a, 'gctx>,
    /// A large collection of information about the result of the entire compilation.
    pub compilation: Compilation<'gctx>,
    /// Output from build scripts, updated after each build script runs.
    pub build_script_outputs: Arc<Mutex<BuildScriptOutputs>>,
    /// Dependencies (like rerun-if-changed) declared by a build script.
    /// This is *only* populated from the output from previous runs.
    /// If the build script hasn't ever been run, then it must be run.
    pub build_explicit_deps: HashMap<Unit, BuildDeps>,
    /// Fingerprints used to detect if a unit is out-of-date.
    pub fingerprints: HashMap<Unit, Arc<Fingerprint>>,
    /// Cache of file mtimes to reduce filesystem hits.
    pub mtime_cache: HashMap<PathBuf, FileTime>,
    /// A set used to track which units have been compiled.
    /// A unit may appear in the job graph multiple times as a dependency of
    /// multiple packages, but it only needs to run once.
    pub compiled: HashSet<Unit>,
    /// Linking information for each `Unit`.
    /// See `build_map` for details.
    pub build_scripts: HashMap<Unit, Arc<BuildScripts>>,
    /// Job server client to manage concurrency with other processes.
    pub jobserver: Client,
    /// "Primary" packages are the ones the user selected on the command-line
    /// with `-p` flags. If no flags are specified, then it is the defaults
    /// based on the current directory and the default workspace members.
    primary_packages: HashSet<PackageId>,
    /// An abstraction of the files and directories that will be generated by
    /// the compilation. This is `None` until after `unit_dependencies` has
    /// been computed.
    files: Option<CompilationFiles<'a, 'gctx>>,

    /// A set of units which are compiling rlibs and are expected to produce
    /// metadata files in addition to the rlib itself.
    rmeta_required: HashSet<Unit>,

    /// Map of the LTO-status of each unit. This indicates what sort of
    /// compilation is happening (only object, only bitcode, both, etc), and is
    /// precalculated early on.
    pub lto: HashMap<Unit, Lto>,

    /// Map of Doc/Docscrape units to metadata for their -Cmetadata flag.
    /// See Context::find_metadata_units for more details.
    pub metadata_for_doc_units: HashMap<Unit, Metadata>,

    /// Set of metadata of Docscrape units that fail before completion, e.g.
    /// because the target has a type error. This is in an Arc<Mutex<..>>
    /// because it is continuously updated as the job progresses.
    pub failed_scrape_units: Arc<Mutex<HashSet<Metadata>>>,
}

impl<'a, 'gctx> BuildRunner<'a, 'gctx> {
    pub fn new(bcx: &'a BuildContext<'a, 'gctx>) -> CargoResult<Self> {
        // Load up the jobserver that we'll use to manage our parallelism. This
        // is the same as the GNU make implementation of a jobserver, and
        // intentionally so! It's hoped that we can interact with GNU make and
        // all share the same jobserver.
        //
        // Note that if we don't have a jobserver in our environment then we
        // create our own, and we create it with `n` tokens, but immediately
        // acquire one, because one token is ourself, a running process.
        let jobserver = match bcx.gctx.jobserver_from_env() {
            Some(c) => c.clone(),
            None => {
                let client = Client::new(bcx.jobs() as usize)
                    .with_context(|| "failed to create jobserver")?;
                client.acquire_raw()?;
                client
            }
        };

        Ok(Self {
            bcx,
            compilation: Compilation::new(bcx)?,
            build_script_outputs: Arc::new(Mutex::new(BuildScriptOutputs::default())),
            fingerprints: HashMap::new(),
            mtime_cache: HashMap::new(),
            compiled: HashSet::new(),
            build_scripts: HashMap::new(),
            build_explicit_deps: HashMap::new(),
            jobserver,
            primary_packages: HashSet::new(),
            files: None,
            rmeta_required: HashSet::new(),
            lto: HashMap::new(),
            metadata_for_doc_units: HashMap::new(),
            failed_scrape_units: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    /// Starts compilation, waits for it to finish, and returns information
    /// about the result of compilation.
    ///
    /// See [`ops::cargo_compile`] for a higher-level view of the compile process.
    ///
    /// [`ops::cargo_compile`]: ../../../ops/cargo_compile/index.html
    #[tracing::instrument(skip_all)]
    pub fn compile(mut self, exec: &Arc<dyn Executor>) -> CargoResult<Compilation<'gctx>> {
        // A shared lock is held during the duration of the build since rustc
        // needs to read from the `src` cache, and we don't want other
        // commands modifying the `src` cache while it is running.
        let _lock = self
            .bcx
            .gctx
            .acquire_package_cache_lock(CacheLockMode::Shared)?;
        let mut queue = JobQueue::new(self.bcx);
        let mut plan = BuildPlan::new();
        let build_plan = self.bcx.build_config.build_plan;
        self.lto = super::lto::generate(self.bcx)?;
        self.prepare_units()?;
        self.prepare()?;
        custom_build::build_map(&mut self)?;
        self.check_collisions()?;
        self.compute_metadata_for_doc_units();

        // We need to make sure that if there were any previous docs
        // already compiled, they were compiled with the same Rustc version that we're currently
        // using. Otherwise we must remove the `doc/` folder and compile again forcing a rebuild.
        //
        // This is important because the `.js`/`.html` & `.css` files that are generated by Rustc don't have
        // any versioning (See https://github.com/rust-lang/cargo/issues/8461).
        // Therefore, we can end up with weird bugs and behaviours if we mix different
        // versions of these files.
        if self.bcx.build_config.mode.is_doc() {
            RustDocFingerprint::check_rustdoc_fingerprint(&self)?
        }

        for unit in &self.bcx.roots {
            let force_rebuild = self.bcx.build_config.force_rebuild;
            super::compile(&mut self, &mut queue, &mut plan, unit, exec, force_rebuild)?;
        }

        // Now that we've got the full job queue and we've done all our
        // fingerprint analysis to determine what to run, bust all the memoized
        // fingerprint hashes to ensure that during the build they all get the
        // most up-to-date values. In theory we only need to bust hashes that
        // transitively depend on a dirty build script, but it shouldn't matter
        // that much for performance anyway.
        for fingerprint in self.fingerprints.values() {
            fingerprint.clear_memoized();
        }

        // Now that we've figured out everything that we're going to do, do it!
        queue.execute(&mut self, &mut plan)?;

        if build_plan {
            plan.set_inputs(self.build_plan_inputs()?);
            plan.output_plan(self.bcx.gctx);
        }

        // Add `OUT_DIR` to env vars if unit has a build script.
        let units_with_build_script = &self
            .bcx
            .roots
            .iter()
            .filter(|unit| self.build_scripts.contains_key(unit))
            .dedup_by(|x, y| x.pkg.package_id() == y.pkg.package_id())
            .collect::<Vec<_>>();
        for unit in units_with_build_script {
            for dep in &self.bcx.unit_graph[unit] {
                if dep.unit.mode.is_run_custom_build() {
                    let out_dir = self
                        .files()
                        .build_script_out_dir(&dep.unit)
                        .display()
                        .to_string();
                    let script_meta = self.get_run_build_script_metadata(&dep.unit);
                    self.compilation
                        .extra_env
                        .entry(script_meta)
                        .or_insert_with(Vec::new)
                        .push(("OUT_DIR".to_string(), out_dir));
                }
            }
        }

        // Collect the result of the build into `self.compilation`.
        for unit in &self.bcx.roots {
            // Collect tests and executables.
            for output in self.outputs(unit)?.iter() {
                if output.flavor == FileFlavor::DebugInfo || output.flavor == FileFlavor::Auxiliary
                {
                    continue;
                }

                let bindst = output.bin_dst();

                if unit.mode == CompileMode::Test {
                    self.compilation
                        .tests
                        .push(self.unit_output(unit, &output.path));
                } else if unit.target.is_executable() {
                    self.compilation
                        .binaries
                        .push(self.unit_output(unit, bindst));
                } else if unit.target.is_cdylib()
                    && !self.compilation.cdylibs.iter().any(|uo| uo.unit == *unit)
                {
                    self.compilation
                        .cdylibs
                        .push(self.unit_output(unit, bindst));
                }
            }

            // Collect information for `rustdoc --test`.
            if unit.mode.is_doc_test() {
                let mut unstable_opts = false;
                let mut args = compiler::extern_args(&self, unit, &mut unstable_opts)?;
                args.extend(compiler::lto_args(&self, unit));
                args.extend(compiler::features_args(unit));
                args.extend(compiler::check_cfg_args(unit)?);

                let script_meta = self.find_build_script_metadata(unit);
                if let Some(meta) = script_meta {
                    if let Some(output) = self.build_script_outputs.lock().unwrap().get(meta) {
                        for cfg in &output.cfgs {
                            args.push("--cfg".into());
                            args.push(cfg.into());
                        }

                        for check_cfg in &output.check_cfgs {
                            args.push("--check-cfg".into());
                            args.push(check_cfg.into());
                        }

                        for (lt, arg) in &output.linker_args {
                            if lt.applies_to(&unit.target) {
                                args.push("-C".into());
                                args.push(format!("link-arg={}", arg).into());
                            }
                        }
                    }
                }
                args.extend(unit.rustdocflags.iter().map(Into::into));

                use super::MessageFormat;
                let format = match self.bcx.build_config.message_format {
                    MessageFormat::Short => "short",
                    MessageFormat::Human => "human",
                    MessageFormat::Json { .. } => "json",
                };
                args.push("--error-format".into());
                args.push(format.into());

                self.compilation.to_doc_test.push(compilation::Doctest {
                    unit: unit.clone(),
                    args,
                    unstable_opts,
                    linker: self.compilation.target_linker(unit.kind).clone(),
                    script_meta,
                    env: artifact::get_env(&self, self.unit_deps(unit))?,
                });
            }

            super::output_depinfo(&mut self, unit)?;

            if self.bcx.build_config.sbom {
                super::output_sbom(&mut self, unit)?;
            }
        }

        for (script_meta, output) in self.build_script_outputs.lock().unwrap().iter() {
            self.compilation
                .extra_env
                .entry(*script_meta)
                .or_insert_with(Vec::new)
                .extend(output.env.iter().cloned());

            for dir in output.library_paths.iter() {
                self.compilation.native_dirs.insert(dir.clone());
            }
        }
        Ok(self.compilation)
    }

    /// Returns the executable for the specified unit (if any).
    pub fn get_executable(&mut self, unit: &Unit) -> CargoResult<Option<PathBuf>> {
        let is_binary = unit.target.is_executable();
        let is_test = unit.mode.is_any_test();
        if !unit.mode.generates_executable() || !(is_binary || is_test) {
            return Ok(None);
        }
        Ok(self
            .outputs(unit)?
            .iter()
            .find(|o| o.flavor == FileFlavor::Normal)
            .map(|output| output.bin_dst().clone()))
    }

    #[tracing::instrument(skip_all)]
    pub fn prepare_units(&mut self) -> CargoResult<()> {
        let dest = self.bcx.profiles.get_dir_name();
        let host_layout = Layout::new(self.bcx.ws, None, &dest)?;
        let mut targets = HashMap::new();
        for kind in self.bcx.all_kinds.iter() {
            if let CompileKind::Target(target) = *kind {
                let layout = Layout::new(self.bcx.ws, Some(target), &dest)?;
                targets.insert(target, layout);
            }
        }
        self.primary_packages
            .extend(self.bcx.roots.iter().map(|u| u.pkg.package_id()));
        self.compilation
            .root_crate_names
            .extend(self.bcx.roots.iter().map(|u| u.target.crate_name()));

        self.record_units_requiring_metadata();

        let files = CompilationFiles::new(self, host_layout, targets);
        self.files = Some(files);
        Ok(())
    }

    /// Prepare this context, ensuring that all filesystem directories are in
    /// place.
    #[tracing::instrument(skip_all)]
    pub fn prepare(&mut self) -> CargoResult<()> {
        self.files
            .as_mut()
            .unwrap()
            .host
            .prepare()
            .with_context(|| "couldn't prepare build directories")?;
        for target in self.files.as_mut().unwrap().target.values_mut() {
            target
                .prepare()
                .with_context(|| "couldn't prepare build directories")?;
        }

        let files = self.files.as_ref().unwrap();
        for &kind in self.bcx.all_kinds.iter() {
            let layout = files.layout(kind);
            self.compilation
                .root_output
                .insert(kind, layout.dest().to_path_buf());
            self.compilation
                .deps_output
                .insert(kind, layout.deps().to_path_buf());
        }
        Ok(())
    }

    pub fn files(&self) -> &CompilationFiles<'a, 'gctx> {
        self.files.as_ref().unwrap()
    }

    /// Returns the filenames that the given unit will generate.
    pub fn outputs(&self, unit: &Unit) -> CargoResult<Arc<Vec<OutputFile>>> {
        self.files.as_ref().unwrap().outputs(unit, self.bcx)
    }

    /// Direct dependencies for the given unit.
    pub fn unit_deps(&self, unit: &Unit) -> &[UnitDep] {
        &self.bcx.unit_graph[unit]
    }

    /// Returns the RunCustomBuild Unit associated with the given Unit.
    ///
    /// If the package does not have a build script, this returns None.
    pub fn find_build_script_unit(&self, unit: &Unit) -> Option<Unit> {
        if unit.mode.is_run_custom_build() {
            return Some(unit.clone());
        }
        self.bcx.unit_graph[unit]
            .iter()
            .find(|unit_dep| {
                unit_dep.unit.mode.is_run_custom_build()
                    && unit_dep.unit.pkg.package_id() == unit.pkg.package_id()
            })
            .map(|unit_dep| unit_dep.unit.clone())
    }

    /// Returns the metadata hash for the RunCustomBuild Unit associated with
    /// the given unit.
    ///
    /// If the package does not have a build script, this returns None.
    pub fn find_build_script_metadata(&self, unit: &Unit) -> Option<Metadata> {
        let script_unit = self.find_build_script_unit(unit)?;
        Some(self.get_run_build_script_metadata(&script_unit))
    }

    /// Returns the metadata hash for a RunCustomBuild unit.
    pub fn get_run_build_script_metadata(&self, unit: &Unit) -> Metadata {
        assert!(unit.mode.is_run_custom_build());
        self.files().metadata(unit)
    }

    /// Returns the list of SBOM output file paths for a given [`Unit`].
    ///
    /// Only call this function when `sbom` is active.
    pub fn sbom_output_files(&self, unit: &Unit) -> CargoResult<Vec<PathBuf>> {
        const SBOM_FILE_EXTENSION: &str = ".cargo-sbom.json";

        fn append_sbom_suffix(link: &PathBuf, suffix: &str) -> PathBuf {
            let mut link_buf = link.clone().into_os_string();
            link_buf.push(suffix);
            PathBuf::from(link_buf)
        }

        assert!(self.bcx.build_config.sbom);
        let files = self
            .outputs(unit)?
            .iter()
            .filter(|o| matches!(o.flavor, FileFlavor::Normal | FileFlavor::Linkable))
            .filter_map(|output_file| output_file.hardlink.as_ref())
            .map(|link| append_sbom_suffix(link, SBOM_FILE_EXTENSION))
            .collect::<Vec<_>>();
        Ok(files)
    }

    pub fn is_primary_package(&self, unit: &Unit) -> bool {
        self.primary_packages.contains(&unit.pkg.package_id())
    }

    /// Returns the list of filenames read by cargo to generate the [`BuildContext`]
    /// (all `Cargo.toml`, etc.).
    pub fn build_plan_inputs(&self) -> CargoResult<Vec<PathBuf>> {
        // Keep sorted for consistency.
        let mut inputs = BTreeSet::new();
        // Note: dev-deps are skipped if they are not present in the unit graph.
        for unit in self.bcx.unit_graph.keys() {
            inputs.insert(unit.pkg.manifest_path().to_path_buf());
        }
        Ok(inputs.into_iter().collect())
    }

    /// Returns a [`UnitOutput`] which represents some information about the
    /// output of a unit.
    pub fn unit_output(&self, unit: &Unit, path: &Path) -> UnitOutput {
        let script_meta = self.find_build_script_metadata(unit);
        UnitOutput {
            unit: unit.clone(),
            path: path.to_path_buf(),
            script_meta,
        }
    }

    /// Check if any output file name collision happens.
    /// See <https://github.com/rust-lang/cargo/issues/6313> for more.
    #[tracing::instrument(skip_all)]
    fn check_collisions(&self) -> CargoResult<()> {
        let mut output_collisions = HashMap::new();
        let describe_collision = |unit: &Unit, other_unit: &Unit, path: &PathBuf| -> String {
            format!(
                "The {} target `{}` in package `{}` has the same output \
                     filename as the {} target `{}` in package `{}`.\n\
                     Colliding filename is: {}\n",
                unit.target.kind().description(),
                unit.target.name(),
                unit.pkg.package_id(),
                other_unit.target.kind().description(),
                other_unit.target.name(),
                other_unit.pkg.package_id(),
                path.display()
            )
        };
        let suggestion =
            "Consider changing their names to be unique or compiling them separately.\n\
             This may become a hard error in the future; see \
             <https://github.com/rust-lang/cargo/issues/6313>.";
        let rustdoc_suggestion =
            "This is a known bug where multiple crates with the same name use\n\
             the same path; see <https://github.com/rust-lang/cargo/issues/6313>.";
        let report_collision = |unit: &Unit,
                                other_unit: &Unit,
                                path: &PathBuf,
                                suggestion: &str|
         -> CargoResult<()> {
            if unit.target.name() == other_unit.target.name() {
                self.bcx.gctx.shell().warn(format!(
                    "output filename collision.\n\
                     {}\
                     The targets should have unique names.\n\
                     {}",
                    describe_collision(unit, other_unit, path),
                    suggestion
                ))
            } else {
                self.bcx.gctx.shell().warn(format!(
                    "output filename collision.\n\
                    {}\
                    The output filenames should be unique.\n\
                    {}\n\
                    If this looks unexpected, it may be a bug in Cargo. Please file a bug report at\n\
                    https://github.com/rust-lang/cargo/issues/ with as much information as you\n\
                    can provide.\n\
                    cargo {} running on `{}` target `{}`\n\
                    First unit: {:?}\n\
                    Second unit: {:?}",
                    describe_collision(unit, other_unit, path),
                    suggestion,
                    crate::version(),
                    self.bcx.host_triple(),
                    self.bcx.target_data.short_name(&unit.kind),
                    unit,
                    other_unit))
            }
        };

        fn doc_collision_error(unit: &Unit, other_unit: &Unit) -> CargoResult<()> {
            bail!(
                "document output filename collision\n\
                 The {} `{}` in package `{}` has the same name as the {} `{}` in package `{}`.\n\
                 Only one may be documented at once since they output to the same path.\n\
                 Consider documenting only one, renaming one, \
                 or marking one with `doc = false` in Cargo.toml.",
                unit.target.kind().description(),
                unit.target.name(),
                unit.pkg,
                other_unit.target.kind().description(),
                other_unit.target.name(),
                other_unit.pkg,
            );
        }

        let mut keys = self
            .bcx
            .unit_graph
            .keys()
            .filter(|unit| !unit.mode.is_run_custom_build())
            .collect::<Vec<_>>();
        // Sort for consistent error messages.
        keys.sort_unstable();
        // These are kept separate to retain compatibility with older
        // versions, which generated an error when there was a duplicate lib
        // or bin (but the old code did not check bin<->lib collisions). To
        // retain backwards compatibility, this only generates an error for
        // duplicate libs or duplicate bins (but not both). Ideally this
        // shouldn't be here, but since there isn't a complete workaround,
        // yet, this retains the old behavior.
        let mut doc_libs = HashMap::new();
        let mut doc_bins = HashMap::new();
        for unit in keys {
            if unit.mode.is_doc() && self.is_primary_package(unit) {
                // These situations have been an error since before 1.0, so it
                // is not a warning like the other situations.
                if unit.target.is_lib() {
                    if let Some(prev) = doc_libs.insert((unit.target.crate_name(), unit.kind), unit)
                    {
                        doc_collision_error(unit, prev)?;
                    }
                } else if let Some(prev) =
                    doc_bins.insert((unit.target.crate_name(), unit.kind), unit)
                {
                    doc_collision_error(unit, prev)?;
                }
            }
            for output in self.outputs(unit)?.iter() {
                if let Some(other_unit) = output_collisions.insert(output.path.clone(), unit) {
                    if unit.mode.is_doc() {
                        // See https://github.com/rust-lang/rust/issues/56169
                        // and https://github.com/rust-lang/rust/issues/61378
                        report_collision(unit, other_unit, &output.path, rustdoc_suggestion)?;
                    } else {
                        report_collision(unit, other_unit, &output.path, suggestion)?;
                    }
                }
                if let Some(hardlink) = output.hardlink.as_ref() {
                    if let Some(other_unit) = output_collisions.insert(hardlink.clone(), unit) {
                        report_collision(unit, other_unit, hardlink, suggestion)?;
                    }
                }
                if let Some(ref export_path) = output.export_path {
                    if let Some(other_unit) = output_collisions.insert(export_path.clone(), unit) {
                        self.bcx.gctx.shell().warn(format!(
                            "`--artifact-dir` filename collision.\n\
                             {}\
                             The exported filenames should be unique.\n\
                             {}",
                            describe_collision(unit, other_unit, export_path),
                            suggestion
                        ))?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Records the list of units which are required to emit metadata.
    ///
    /// Units which depend only on the metadata of others requires the others to
    /// actually produce metadata, so we'll record that here.
    fn record_units_requiring_metadata(&mut self) {
        for (key, deps) in self.bcx.unit_graph.iter() {
            for dep in deps {
                if self.only_requires_rmeta(key, &dep.unit) {
                    self.rmeta_required.insert(dep.unit.clone());
                }
            }
        }
    }

    /// Returns whether when `parent` depends on `dep` if it only requires the
    /// metadata file from `dep`.
    pub fn only_requires_rmeta(&self, parent: &Unit, dep: &Unit) -> bool {
        // We're only a candidate for requiring an `rmeta` file if we
        // ourselves are building an rlib,
        !parent.requires_upstream_objects()
            && parent.mode == CompileMode::Build
            // Our dependency must also be built as an rlib, otherwise the
            // object code must be useful in some fashion
            && !dep.requires_upstream_objects()
            && dep.mode == CompileMode::Build
    }

    /// Returns whether when `unit` is built whether it should emit metadata as
    /// well because some compilations rely on that.
    pub fn rmeta_required(&self, unit: &Unit) -> bool {
        self.rmeta_required.contains(unit)
    }

    /// Finds metadata for Doc/Docscrape units.
    ///
    /// rustdoc needs a -Cmetadata flag in order to recognize StableCrateIds that refer to
    /// items in the crate being documented. The -Cmetadata flag used by reverse-dependencies
    /// will be the metadata of the Cargo unit that generated the current library's rmeta file,
    /// which should be a Check unit.
    ///
    /// If the current crate has reverse-dependencies, such a Check unit should exist, and so
    /// we use that crate's metadata. If not, we use the crate's Doc unit so at least examples
    /// scraped from the current crate can be used when documenting the current crate.
    #[tracing::instrument(skip_all)]
    pub fn compute_metadata_for_doc_units(&mut self) {
        for unit in self.bcx.unit_graph.keys() {
            if !unit.mode.is_doc() && !unit.mode.is_doc_scrape() {
                continue;
            }

            let matching_units = self
                .bcx
                .unit_graph
                .keys()
                .filter(|other| {
                    unit.pkg == other.pkg
                        && unit.target == other.target
                        && !other.mode.is_doc_scrape()
                })
                .collect::<Vec<_>>();
            let metadata_unit = matching_units
                .iter()
                .find(|other| other.mode.is_check())
                .or_else(|| matching_units.iter().find(|other| other.mode.is_doc()))
                .unwrap_or(&unit);
            self.metadata_for_doc_units
                .insert(unit.clone(), self.files().metadata(metadata_unit));
        }
    }
}
