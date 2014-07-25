use std::collections::HashMap;
use std::vec::Vec;
use core::{Source, SourceId, SourceMap, Summary, Dependency, PackageId, Package};
use util::{CargoResult, ChainError, Config, human};

pub trait Registry {
    fn query(&mut self, name: &Dependency) -> CargoResult<Vec<Summary>>;
}

impl Registry for Vec<Summary> {
    fn query(&mut self, dep: &Dependency) -> CargoResult<Vec<Summary>> {
        Ok(self.iter()
            .filter(|summary| dep.matches(*summary))
            .map(|summary| summary.clone())
            .collect())
    }
}

pub struct PackageRegistry<'a> {
    sources: SourceMap,
    overrides: Vec<Summary>,
    summaries: Vec<Summary>,
    config: &'a mut Config<'a>
}

impl<'a> PackageRegistry<'a> {
    pub fn new<'a>(source_ids: Vec<SourceId>,
               override_ids: Vec<SourceId>,
               config: &'a mut Config<'a>) -> CargoResult<PackageRegistry<'a>> {

        let mut reg = PackageRegistry::empty(config);
        let source_ids = dedup(source_ids);

        for id in source_ids.iter() {
            try!(reg.load(id, false));
        }

        for id in override_ids.iter() {
            try!(reg.load(id, true));
        }

        Ok(reg)
    }

    fn empty<'a>(config: &'a mut Config<'a>) -> PackageRegistry<'a> {
        PackageRegistry {
            sources: SourceMap::new(),
            overrides: vec!(),
            summaries: vec!(),
            config: config
        }
    }

    pub fn get(&self, package_ids: &[PackageId]) -> CargoResult<Vec<Package>> {
        log!(5, "getting packags; sources={}; ids={}", self.sources.len(),
             package_ids);

        // TODO: Only call source with package ID if the package came from the
        // source
        let mut ret = Vec::new();

        for source in self.sources.sources() {
            try!(source.download(package_ids));
            let packages = try!(source.get(package_ids));

            ret.push_all_move(packages);
        }

        // TODO: Return earlier if fail
        assert!(package_ids.len() == ret.len(),
                "could not get packages from registry; ids={}", package_ids);

        Ok(ret)
    }

    pub fn move_sources(self) -> SourceMap {
        self.sources
    }

    fn ensure_loaded(&mut self, namespace: &SourceId) -> CargoResult<()> {
        if self.sources.contains(namespace) {
            return Ok(());
        }

        try!(self.load(namespace, false));
        Ok(())
    }

    fn load(&mut self, namespace: &SourceId, override: bool) -> CargoResult<()> {
        (|| {
            let mut source = namespace.load(self.config);
            let dst = if override {&mut self.overrides} else {&mut self.summaries};

            // Ensure the source has fetched all necessary remote data.
            try!(source.update());

            // Get the summaries
            for summary in (try!(source.list())).iter() {
                assert!(!dst.contains(summary), "duplicate summaries: {}", summary);
                dst.push(summary.clone());
                // self.summaries.push(summary.clone());
            }

            // Save off the source
            self.sources.insert(namespace, source);

            Ok(())
        }).chain_error(|| human(format!("Unable to update {}", namespace)))
    }

    fn query_overrides(&self, dep: &Dependency) -> Vec<Summary> {
        self.overrides.iter()
            .filter(|s| s.get_name() == dep.get_name())
            .map(|s| s.clone())
            .collect()
    }
}

fn dedup(ids: Vec<SourceId>) -> Vec<SourceId> {
    let mut seen = vec!();

    for id in ids.move_iter() {
        if seen.contains(&id) { continue; }
        seen.push(id);
    }

    seen
}

impl<'a> Registry for PackageRegistry<'a> {
    fn query(&mut self, dep: &Dependency) -> CargoResult<Vec<Summary>> {
        let overrides = self.query_overrides(dep);

        if overrides.is_empty() {
            // Ensure the requested namespace is loaded
            try!(self.ensure_loaded(dep.get_namespace()));
            self.summaries.query(dep)
        } else {
            Ok(overrides)
        }
    }
}
