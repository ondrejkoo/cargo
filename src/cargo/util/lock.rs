use file_lock::filename::{Mode, ParseError};
use file_lock::filename::Lock as FileLock;
use file_lock::filename::Error as FileLockError;
use file_lock::fd::Error as LockError;
use errno;
use libc;

use std::path::PathBuf;
use std::fs;
use std::thread::sleep_ms;

use util::{Config, CargoError, CargoResult, human, caused_human};

pub use file_lock::filename::Kind as LockKind;

impl From<FileLockError> for Box<CargoError> {
    fn from(t: FileLockError) -> Self {
        Box::new(t)
    }
}

impl From<ParseError> for Box<CargoError> {
    fn from(t: ParseError) -> Self {
        Box::new(t)
    }
}

impl CargoError for FileLockError {
    fn is_human(&self) -> bool { true }
}
impl CargoError for ParseError {}

pub struct CargoLock {
    kind: LockKind, 
    inner: FileLock,
}

impl CargoLock {

    pub fn lock_kind(config: &Config) -> CargoResult<LockKind> {
        // TODO(ST): rename config key to something more suitable
        const CONFIG_KEY: &'static str = "build.lock-kind";
        match try!(config.get_string(CONFIG_KEY)).map(|t| t.0) {
            None => Ok(LockKind::NonBlocking),
            Some(kind_string) => match kind_string.parse() {
                Ok(kind) => Ok(kind),
                Err(_) => Err(human(format!("Failed to parse value '{}' at configuration key \
                                            '{}'. Must be one of '{}' and '{}'",
                                            kind_string, CONFIG_KEY,
                                            LockKind::NonBlocking.as_ref(), 
                                            LockKind::Blocking.as_ref())))
            }
        }
    }

    pub fn new(path: PathBuf, kind: LockKind) -> CargoLock {
        CargoLock {
            kind: kind,
            inner: FileLock::new(path, Mode::Write)
        }
    }

    pub fn new_shared(path: PathBuf, kind: LockKind) -> CargoLock {
        CargoLock {
            kind: kind,
            inner: FileLock::new(path, Mode::Read)
        }
    }

    pub fn lock(&mut self) -> CargoResult<()> {
        // NOTE(ST): This could fail if cargo is run concurrently for the first time
        // The only way to prevent it would be to take a lock in a directory which exists.
        // This is why we don't try! here, but hope the directory exists when we 
        // try to create the lock file
        {
            let lock_dir = self.inner.path().parent().unwrap();
            if let Err(_) = fs::create_dir_all(lock_dir) {
                // We might compete to create one or more directories here
                // Give the competing process some time to finish. Then we will
                // retry, hoping it the creation works (maybe just because the )
                // directory is available already.
                // TODO(ST): magic numbers, especially in sleep, will fail at some point ... .
                sleep_ms(100);
                if let Err(io_err) = fs::create_dir_all(lock_dir) {
                    // Fail permanently if it still didn't work ... 
                    return Err(caused_human(format!("Failed to create parent directory of \
                                                     lock-file at '{}'", 
                                                     lock_dir.display()), io_err));
                }
            }
        }
        debug!("About to acquire file lock: '{}'", self.inner.path().display());
        // TODO(ST): This evil hack will just retry until we have the lock or 
        //           fail with an error that is no "Deadlock avoided".
        //           When there is high intra-process contention thanks to threads,
        //           this issue occours even though I would hope that we don't try to 
        //           to have multiple threads obtain a lock on the same lock file.
        //           However, apparently this does happen.
        //           Even if it does I don't understand why this is a deadlock for him.
        //           The good news is that building now works in my manual tests.
        loop {
            match self.inner.any_lock(self.kind.clone()) {
                Err(FileLockError::LockError(
                        LockError::Errno(
                            errno::Errno(libc::consts::os::posix88::EDEADLK)))) 
                    => { 
                        sleep_ms(100);
                        continue
                },
                Err(any_err) => return Err(any_err.into()),
                Ok(()) => return Ok(())
            }
        }
    }
}
