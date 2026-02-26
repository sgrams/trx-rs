// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use std::collections::HashMap;

use trx_core::rig::RigCat;
use trx_core::DynResult;

mod dummy;

#[cfg(feature = "ft450d")]
use trx_backend_ft450d::Ft450d;
#[cfg(feature = "ft817")]
use trx_backend_ft817::Ft817;
#[cfg(feature = "soapysdr")]
pub use trx_backend_soapysdr::SoapySdrRig;

/// Connection details for instantiating a rig backend.
#[derive(Debug, Clone)]
pub enum RigAccess {
    Serial { path: String, baud: u32 },
    Tcp { addr: String },
    Sdr { args: String },
}

pub type BackendFactory = fn(RigAccess) -> DynResult<Box<dyn RigCat>>;

/// Context for registering and instantiating rig backends.
#[derive(Clone)]
pub struct RegistrationContext {
    factories: HashMap<String, BackendFactory>,
}

impl RegistrationContext {
    /// Create a new empty registration context.
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Register a backend factory under a stable name (e.g. "ft817").
    pub fn register_backend(&mut self, name: &str, factory: BackendFactory) {
        let key = normalize_name(name);
        self.factories.insert(key, factory);
    }

    /// Check whether a backend name is registered.
    pub fn is_backend_registered(&self, name: &str) -> bool {
        let key = normalize_name(name);
        self.factories.contains_key(&key)
    }

    /// List registered backend names.
    pub fn registered_backends(&self) -> Vec<String> {
        let mut names: Vec<String> = self.factories.keys().cloned().collect();
        names.sort();
        names
    }

    /// Instantiate a rig backend based on the selected name and access method.
    pub fn build_rig(&self, name: &str, access: RigAccess) -> DynResult<Box<dyn RigCat>> {
        let key = normalize_name(name);
        let factory = self
            .factories
            .get(&key)
            .ok_or_else(|| format!("Unknown rig backend: {}", name))?;
        factory(access)
    }

    /// Merge another registration context into this one.
    pub fn extend_from(&mut self, other: &RegistrationContext) {
        for (name, factory) in &other.factories {
            self.factories.insert(name.clone(), *factory);
        }
    }
}

impl Default for RegistrationContext {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_name(name: &str) -> String {
    name.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Register all built-in backends enabled by features on a context.
pub fn register_builtin_backends_on(context: &mut RegistrationContext) {
    context.register_backend("dummy", dummy_factory);
    #[cfg(feature = "ft817")]
    context.register_backend("ft817", ft817_factory);
    #[cfg(feature = "ft450d")]
    context.register_backend("ft450d", ft450d_factory);
    #[cfg(feature = "soapysdr")]
    context.register_backend("soapysdr", soapysdr_factory);
}

fn dummy_factory(_access: RigAccess) -> DynResult<Box<dyn RigCat>> {
    Ok(Box::new(dummy::DummyRig::new()))
}

#[cfg(feature = "ft817")]
fn ft817_factory(access: RigAccess) -> DynResult<Box<dyn RigCat>> {
    match access {
        RigAccess::Serial { path, baud } => Ok(Box::new(Ft817::new(&path, baud)?)),
        RigAccess::Tcp { .. } => Err("FT-817 only supports serial CAT access".into()),
        RigAccess::Sdr { .. } => Err("FT-817 only supports serial CAT access".into()),
    }
}

#[cfg(feature = "ft450d")]
fn ft450d_factory(access: RigAccess) -> DynResult<Box<dyn RigCat>> {
    match access {
        RigAccess::Serial { path, baud } => Ok(Box::new(Ft450d::new(&path, baud)?)),
        RigAccess::Tcp { .. } => Err("FT-450D only supports serial CAT access".into()),
        RigAccess::Sdr { .. } => Err("FT-450D only supports serial CAT access".into()),
    }
}

#[cfg(feature = "soapysdr")]
fn soapysdr_factory(access: RigAccess) -> DynResult<Box<dyn RigCat>> {
    match access {
        RigAccess::Sdr { args } => Ok(Box::new(trx_backend_soapysdr::SoapySdrRig::new(&args)?)),
        _ => Err("soapysdr backend requires Sdr access type".into()),
    }
}
