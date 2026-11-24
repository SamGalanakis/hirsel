//! Hosts without cgroup v2 and Linux namespaces cannot contain: [`Host::open`]
//! always refuses, so no [`Host`] and no [`Contained`] child ever exists.

use super::*;
use std::convert::Infallible;

pub struct Host(Infallible);

impl Host {
    /// Refuses: this host has no containment mechanism.
    pub fn open(_root: &Path, _state: &Path) -> Result<Self, Uncontainable> {
        Err(Uncontainable::Mechanism {
            step: "host",
            errno: libc::ENOSYS,
        })
    }

    pub fn recover(&self) -> io::Result<Vec<ChildId>> {
        match self.0 {}
    }

    pub fn launch(&self, _launch: Launch<'_>) -> Result<Contained, LaunchError> {
        match self.0 {}
    }
}

pub struct Contained(Infallible);

impl Contained {
    pub fn pidfd(&self) -> std::os::fd::BorrowedFd<'_> {
        match self.0 {}
    }

    pub fn pid(&self) -> i32 {
        match self.0 {}
    }

    pub fn cgroup(&self) -> &Path {
        match self.0 {}
    }

    pub fn signal(&self, _signal: i32) -> io::Result<()> {
        match self.0 {}
    }

    pub fn kill(&self) -> io::Result<()> {
        match self.0 {}
    }

    pub fn wait(self) -> io::Result<End> {
        match self.0 {}
    }
}
