//! A simple disk usage estimator.

use anyhow::{Context, Result};
use ignore::overrides::OverrideBuilder;
use ignore::{WalkBuilder, WalkState};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[cfg(target_has_atomic = "64")]
use std::sync::atomic::{AtomicU64, Ordering};

/// Determines the disk usage of all files in the given directory.
///
/// The given patterns are gitignore style patterns relative to the given
/// path. If there are patterns, it will only count things matching that
/// pattern. `!` can be used to exclude things. See [`OverrideBuilder::add`]
/// for more info.
///
/// This is a primitive implementation that doesn't handle hard links, and
/// isn't particularly fast (for example, not using `getattrlistbulk` on
/// macOS). It also only uses actual byte sizes instead of block counts (and
/// thus vastly undercounts directories with lots of small files). It would be
/// nice to improve this or replace it with something better.
pub fn du(path: &Path, patterns: &[&str]) -> Result<u64> {
    du_inner(path, patterns).with_context(|| format!("failed to walk `{}`", path.display()))
}

fn du_inner(path: &Path, patterns: &[&str]) -> Result<u64> {
    let mut builder = OverrideBuilder::new(path);
    for pattern in patterns {
        builder.add(pattern)?;
    }
    let overrides = builder.build()?;

    let mut builder = WalkBuilder::new(path);
    builder
        .overrides(overrides)
        .hidden(false)
        .parents(false)
        .ignore(false)
        .git_global(false)
        .git_ignore(false)
        .git_exclude(false);
    let walker = builder.build_parallel();

    // Not all targets support atomics, so a mutex is used as a fallback
    // https://github.com/rust-lang/cargo/pull/12981
    // https://doc.rust-lang.org/std/sync/atomic/index.html#portability
    #[cfg(target_has_atomic = "64")]
    let total = AtomicU64::new(0);

    #[cfg(not(target_has_atomic = "64"))]
    let total = Arc::new(Mutex::new(0_u64));

    // A slot used to indicate there was an error while walking.
    //
    // It is possible that more than one error happens (such as in different
    // threads). The error returned is arbitrary in that case.
    let err = Arc::new(Mutex::new(None));
    walker.run(|| {
        Box::new(|entry| {
            match entry {
                Ok(entry) => match entry.metadata() {
                    Ok(meta) => {
                        if meta.is_file() {
                            #[cfg(target_has_atomic = "64")]
                            {
                                // Note that fetch_add may wrap the u64.
                                total.fetch_add(meta.len(), Ordering::Relaxed);
                            }

                            #[cfg(not(target_has_atomic = "64"))]
                            {
                                let mut lock = total.lock().unwrap();
                                *lock += meta.len();
                            }
                        }
                    }
                    Err(e) => {
                        *err.lock().unwrap() = Some(e.into());
                        return WalkState::Quit;
                    }
                },
                Err(e) => {
                    *err.lock().unwrap() = Some(e.into());
                    return WalkState::Quit;
                }
            }
            WalkState::Continue
        })
    });

    if let Some(e) = err.lock().unwrap().take() {
        return Err(e);
    }

    #[cfg(target_has_atomic = "64")]
    return Ok(total.load(Ordering::Relaxed));

    #[cfg(not(target_has_atomic = "64"))]
    Ok(*total.lock().unwrap())
}
