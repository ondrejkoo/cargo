#![allow(deprecated)]
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use filetime::FileTime;
use jobserver::Client;

use crate::core::compiler::{self, compilation, Unit};
use crate::core::PackageId;
use crate::util::errors::{CargoResult, CargoResultExt};
use crate::util::{profile, Config};

use super::build_plan::BuildPlan;
use super::custom_build::{self, BuildDeps, BuildScriptOutputs, BuildScripts};
use super::fingerprint::Fingerprint;
use super::job_queue::JobQueue;
use super::layout::Layout;
use super::unit_graph::{UnitDep, UnitGraph};
use super::{BuildContext, Compilation, CompileKind, CompileMode, Executor, FileFlavor};

mod compilation_files;
use self::compilation_files::CompilationFiles;
pub use self::compilation_files::{Metadata, OutputFile};

/// Collection of all the stuff that is needed to perform a build.
pub struct Context<'a, 'cfg> {
    /// Mostly static information about the build task.
    pub bcx: &'a BuildContext<'a, 'cfg>,
    /// A large collection of information about the result of the entire compilation.
    pub compilation: Compilation<'cfg>,
    /// Output from build scripts, updated after each build script runs.
    pub build_script_outputs: Arc<Mutex<BuildScriptOutputs>>,
    /// Dependencies (like rerun-if-changed) declared by a build script.
    /// This is *only* populated from the output from previous runs.
    /// If the build script hasn't ever been run, then it must be run.
    pub build_explicit_deps: HashMap<Unit<'a>, BuildDeps>,
    /// Fingerprints used to detect if a unit is out-of-date.
    pub fingerprints: HashMap<Unit<'a>, Arc<Fingerprint>>,
    /// Cache of file mtimes to reduce filesystem hits.
    pub mtime_cache: HashMap<PathBuf, FileTime>,
    /// A set used to track which units have been compiled.
    /// A unit may appear in the job graph multiple times as a dependency of
    /// multiple packages, but it only needs to run once.
    pub compiled: HashSet<Unit<'a>>,
    /// Linking information for each `Unit`.
    /// See `build_map` for details.
    pub build_scripts: HashMap<Unit<'a>, Arc<BuildScripts>>,
    /// Job server client to manage concurrency with other processes.
    pub jobserver: Client,
    /// "Primary" packages are the ones the user selected on the command-line
    /// with `-p` flags. If no flags are specified, then it is the defaults
    /// based on the current directory and the default workspace members.
    primary_packages: HashSet<PackageId>,
    /// The dependency graph of units to compile.
    unit_dependencies: UnitGraph<'a>,
    /// An abstraction of the files and directories that will be generated by
    /// the compilation. This is `None` until after `unit_dependencies` has
    /// been computed.
    files: Option<CompilationFiles<'a, 'cfg>>,

    /// A flag indicating whether pipelining is enabled for this compilation
    /// session. Pipelining largely only affects the edges of the dependency
    /// graph that we generate at the end, and otherwise it's pretty
    /// straightforward.
    pipelining: bool,

    /// A set of units which are compiling rlibs and are expected to produce
    /// metadata files in addition to the rlib itself. This is only filled in
    /// when `pipelining` above is enabled.
    rmeta_required: HashSet<Unit<'a>>,

    /// When we're in jobserver-per-rustc process mode, this keeps those
    /// jobserver clients for each Unit (which eventually becomes a rustc
    /// process).
    pub rustc_clients: HashMap<Unit<'a>, Client>,
}

impl<'a, 'cfg> Context<'a, 'cfg> {
    pub fn new(
        config: &'cfg Config,
        bcx: &'a BuildContext<'a, 'cfg>,
        unit_dependencies: UnitGraph<'a>,
        default_kind: CompileKind,
    ) -> CargoResult<Self> {
        // Load up the jobserver that we'll use to manage our parallelism. This
        // is the same as the GNU make implementation of a jobserver, and
        // intentionally so! It's hoped that we can interact with GNU make and
        // all share the same jobserver.
        //
        // Note that if we don't have a jobserver in our environment then we
        // create our own, and we create it with `n` tokens, but immediately
        // acquire one, because one token is ourself, a running process.
        let jobserver = match config.jobserver_from_env() {
            Some(c) => c.clone(),
            None => {
                let client = Client::new(bcx.build_config.jobs as usize)
                    .chain_err(|| "failed to create jobserver")?;
                client.acquire_raw()?;
                client
            }
        };

        let pipelining = bcx.config.build_config()?.pipelining.unwrap_or(true);

        Ok(Self {
            bcx,
            compilation: Compilation::new(bcx, default_kind)?,
            build_script_outputs: Arc::new(Mutex::new(BuildScriptOutputs::default())),
            fingerprints: HashMap::new(),
            mtime_cache: HashMap::new(),
            compiled: HashSet::new(),
            build_scripts: HashMap::new(),
            build_explicit_deps: HashMap::new(),
            jobserver,
            primary_packages: HashSet::new(),
            unit_dependencies,
            files: None,
            rmeta_required: HashSet::new(),
            rustc_clients: HashMap::new(),
            pipelining,
        })
    }

    /// Starts compilation, waits for it to finish, and returns information
    /// about the result of compilation.
    pub fn compile(
        mut self,
        units: &[Unit<'a>],
        export_dir: Option<PathBuf>,
        exec: &Arc<dyn Executor>,
        exclude_project_sources: bool,
    ) -> CargoResult<Compilation<'cfg>> {
        let mut queue = JobQueue::new(self.bcx, units);
        let mut plan = BuildPlan::new();
        let build_plan = self.bcx.build_config.build_plan;
        self.prepare_units(export_dir, units)?;
        self.prepare()?;
        custom_build::build_map(&mut self, units)?;
        self.check_collistions()?;

        for unit in units.iter() {
            // Build up a list of pending jobs, each of which represent
            // compiling a particular package. No actual work is executed as
            // part of this, that's all done next as part of the `execute`
            // function which will run everything in order with proper
            // parallelism.
            let force_rebuild = self.bcx.build_config.force_rebuild;
            super::compile(
                &mut self,
                &mut queue,
                &mut plan,
                unit,
                exec,
                force_rebuild,
                exclude_project_sources,
            )?;
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
            plan.output_plan();
        }

        // Collect the result of the build into `self.compilation`.
        for unit in units.iter() {
            // Collect tests and executables.
            for output in self.outputs(unit)?.iter() {
                if output.flavor == FileFlavor::DebugInfo || output.flavor == FileFlavor::Auxiliary
                {
                    continue;
                }

                let bindst = output.bin_dst();

                if unit.mode == CompileMode::Test {
                    self.compilation.tests.push((
                        unit.pkg.clone(),
                        unit.target.clone(),
                        output.path.clone(),
                    ));
                } else if unit.target.is_executable() {
                    self.compilation.binaries.push(bindst.clone());
                }
            }

            // If the unit has a build script, add `OUT_DIR` to the
            // environment variables.
            if unit.target.is_lib() {
                for dep in &self.unit_dependencies[unit] {
                    if dep.unit.mode.is_run_custom_build() {
                        let out_dir = self
                            .files()
                            .build_script_out_dir(&dep.unit)
                            .display()
                            .to_string();
                        self.compilation
                            .extra_env
                            .entry(dep.unit.pkg.package_id())
                            .or_insert_with(Vec::new)
                            .push(("OUT_DIR".to_string(), out_dir));
                    }
                }
            }

            // Collect information for `rustdoc --test`.
            if unit.mode.is_doc_test() {
                let mut unstable_opts = false;
                let args = compiler::extern_args(&self, unit, &mut unstable_opts)?;
                self.compilation.to_doc_test.push(compilation::Doctest {
                    package: unit.pkg.clone(),
                    target: unit.target.clone(),
                    args,
                    unstable_opts,
                });
            }

            // Collect the enabled features.
            let feats = &unit.features;
            if !feats.is_empty() {
                self.compilation
                    .cfgs
                    .entry(unit.pkg.package_id())
                    .or_insert_with(|| {
                        feats
                            .iter()
                            .map(|feat| format!("feature=\"{}\"", feat))
                            .collect()
                    });
            }

            // Collect rustdocflags.
            let rustdocflags = self.bcx.rustdocflags_args(unit);
            if !rustdocflags.is_empty() {
                self.compilation
                    .rustdocflags
                    .entry(unit.pkg.package_id())
                    .or_insert_with(|| rustdocflags.to_vec());
            }

            super::output_depinfo(&mut self, unit)?;
        }

        for (pkg_id, output) in self.build_script_outputs.lock().unwrap().iter() {
            self.compilation
                .cfgs
                .entry(pkg_id)
                .or_insert_with(HashSet::new)
                .extend(output.cfgs.iter().cloned());

            self.compilation
                .extra_env
                .entry(pkg_id)
                .or_insert_with(Vec::new)
                .extend(output.env.iter().cloned());

            for dir in output.library_paths.iter() {
                self.compilation.native_dirs.insert(dir.clone());
            }
        }
        Ok(self.compilation)
    }

    /// Returns the executable for the specified unit (if any).
    pub fn get_executable(&mut self, unit: &Unit<'a>) -> CargoResult<Option<PathBuf>> {
        for output in self.outputs(unit)?.iter() {
            if output.flavor == FileFlavor::DebugInfo {
                continue;
            }

            let is_binary = unit.target.is_executable();
            let is_test = unit.mode.is_any_test() && !unit.mode.is_check();

            if is_binary || is_test {
                return Ok(Option::Some(output.bin_dst().clone()));
            }
        }
        Ok(None)
    }

    pub fn prepare_units(
        &mut self,
        export_dir: Option<PathBuf>,
        units: &[Unit<'a>],
    ) -> CargoResult<()> {
        let dest = self.bcx.profiles.get_dir_name();
        let host_layout = Layout::new(self.bcx.ws, None, &dest)?;
        let mut targets = HashMap::new();
        if let CompileKind::Target(target) = self.bcx.build_config.requested_kind {
            let layout = Layout::new(self.bcx.ws, Some(target), &dest)?;
            targets.insert(target, layout);
        }
        self.primary_packages
            .extend(units.iter().map(|u| u.pkg.package_id()));

        self.record_units_requiring_metadata();

        let files =
            CompilationFiles::new(units, host_layout, targets, export_dir, self.bcx.ws, self);
        self.files = Some(files);
        Ok(())
    }

    /// Prepare this context, ensuring that all filesystem directories are in
    /// place.
    pub fn prepare(&mut self) -> CargoResult<()> {
        let _p = profile::start("preparing layout");

        self.files_mut()
            .host
            .prepare()
            .chain_err(|| "couldn't prepare build directories")?;
        for target in self.files.as_mut().unwrap().target.values_mut() {
            target
                .prepare()
                .chain_err(|| "couldn't prepare build directories")?;
        }

        self.compilation.host_deps_output = self.files_mut().host.deps().to_path_buf();

        let files = self.files.as_ref().unwrap();
        let layout = files.layout(self.bcx.build_config.requested_kind);
        self.compilation.root_output = layout.dest().to_path_buf();
        self.compilation.deps_output = layout.deps().to_path_buf();
        Ok(())
    }

    pub fn files(&self) -> &CompilationFiles<'a, 'cfg> {
        self.files.as_ref().unwrap()
    }

    fn files_mut(&mut self) -> &mut CompilationFiles<'a, 'cfg> {
        self.files.as_mut().unwrap()
    }

    /// Returns the filenames that the given unit will generate.
    pub fn outputs(&self, unit: &Unit<'a>) -> CargoResult<Arc<Vec<OutputFile>>> {
        self.files.as_ref().unwrap().outputs(unit, self.bcx)
    }

    /// Direct dependencies for the given unit.
    pub fn unit_deps(&self, unit: &Unit<'a>) -> &[UnitDep<'a>] {
        &self.unit_dependencies[unit]
    }

    /// Returns the RunCustomBuild Unit associated with the given Unit.
    ///
    /// If the package does not have a build script, this returns None.
    pub fn find_build_script_unit(&self, unit: Unit<'a>) -> Option<Unit<'a>> {
        if unit.mode.is_run_custom_build() {
            return Some(unit);
        }
        self.unit_dependencies[&unit]
            .iter()
            .find(|unit_dep| {
                unit_dep.unit.mode.is_run_custom_build()
                    && unit_dep.unit.pkg.package_id() == unit.pkg.package_id()
            })
            .map(|unit_dep| unit_dep.unit)
    }

    /// Returns the metadata hash for the RunCustomBuild Unit associated with
    /// the given unit.
    ///
    /// If the package does not have a build script, this returns None.
    pub fn find_build_script_metadata(&self, unit: Unit<'a>) -> Option<Metadata> {
        let script_unit = self.find_build_script_unit(unit)?;
        Some(self.get_run_build_script_metadata(&script_unit))
    }

    /// Returns the metadata hash for a RunCustomBuild unit.
    pub fn get_run_build_script_metadata(&self, unit: &Unit<'a>) -> Metadata {
        assert!(unit.mode.is_run_custom_build());
        self.files()
            .metadata(unit)
            .expect("build script should always have hash")
    }

    pub fn is_primary_package(&self, unit: &Unit<'a>) -> bool {
        self.primary_packages.contains(&unit.pkg.package_id())
    }

    /// Returns the list of filenames read by cargo to generate the `BuildContext`
    /// (all `Cargo.toml`, etc.).
    pub fn build_plan_inputs(&self) -> CargoResult<Vec<PathBuf>> {
        // Keep sorted for consistency.
        let mut inputs = BTreeSet::new();
        // Note: dev-deps are skipped if they are not present in the unit graph.
        for unit in self.unit_dependencies.keys() {
            inputs.insert(unit.pkg.manifest_path().to_path_buf());
        }
        Ok(inputs.into_iter().collect())
    }

    fn check_collistions(&self) -> CargoResult<()> {
        let mut output_collisions = HashMap::new();
        let describe_collision =
            |unit: &Unit<'_>, other_unit: &Unit<'_>, path: &PathBuf| -> String {
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
        let report_collision = |unit: &Unit<'_>,
                                other_unit: &Unit<'_>,
                                path: &PathBuf,
                                suggestion: &str|
         -> CargoResult<()> {
            if unit.target.name() == other_unit.target.name() {
                self.bcx.config.shell().warn(format!(
                    "output filename collision.\n\
                     {}\
                     The targets should have unique names.\n\
                     {}",
                    describe_collision(unit, other_unit, path),
                    suggestion
                ))
            } else {
                self.bcx.config.shell().warn(format!(
                    "output filename collision.\n\
                    {}\
                    The output filenames should be unique.\n\
                    {}\n\
                    If this looks unexpected, it may be a bug in Cargo. Please file a bug report at\n\
                    https://github.com/rust-lang/cargo/issues/ with as much information as you\n\
                    can provide.\n\
                    {} running on `{}` target `{}`\n\
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

        let mut keys = self
            .unit_dependencies
            .keys()
            .filter(|unit| !unit.mode.is_run_custom_build())
            .collect::<Vec<_>>();
        // Sort for consistent error messages.
        keys.sort_unstable();
        for unit in keys {
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
                        self.bcx.config.shell().warn(format!(
                            "`--out-dir` filename collision.\n\
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
        for (key, deps) in self.unit_dependencies.iter() {
            for dep in deps {
                if self.only_requires_rmeta(key, &dep.unit) {
                    self.rmeta_required.insert(dep.unit);
                }
            }
        }
    }

    /// Returns whether when `parent` depends on `dep` if it only requires the
    /// metadata file from `dep`.
    pub fn only_requires_rmeta(&self, parent: &Unit<'a>, dep: &Unit<'a>) -> bool {
        // this is only enabled when pipelining is enabled
        self.pipelining
            // We're only a candidate for requiring an `rmeta` file if we
            // ourselves are building an rlib,
            && !parent.requires_upstream_objects()
            && parent.mode == CompileMode::Build
            // Our dependency must also be built as an rlib, otherwise the
            // object code must be useful in some fashion
            && !dep.requires_upstream_objects()
            && dep.mode == CompileMode::Build
    }

    /// Returns whether when `unit` is built whether it should emit metadata as
    /// well because some compilations rely on that.
    pub fn rmeta_required(&self, unit: &Unit<'a>) -> bool {
        self.rmeta_required.contains(unit) || self.bcx.config.cli_unstable().timings.is_some()
    }

    pub fn new_jobserver(&mut self) -> CargoResult<Client> {
        let tokens = self.bcx.build_config.jobs as usize;
        let client = Client::new(tokens).chain_err(|| "failed to create jobserver")?;

        // Drain the client fully
        for i in 0..tokens {
            client.acquire_raw().chain_err(|| {
                format!(
                    "failed to fully drain {}/{} token from jobserver at startup",
                    i, tokens,
                )
            })?;
        }

        Ok(client)
    }
}
