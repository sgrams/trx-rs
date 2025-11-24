// SPDX-FileCopyrightText: 2025 Stanislaw Grams <stanislawgrams@gmail.com>
//
// SPDX-License-Identifier: BSD-2-Clause

use clap::ValueEnum;
use trx_core::rig::RigCat;
use trx_core::DynResult;

#[cfg(feature = "ft817")]
use trx_backend_ft817::Ft817;

/// Supported rig backends selectable at runtime.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RigKind {
    #[cfg(feature = "ft817")]
    #[value(alias = "ft-817")]
    Ft817,
}

impl RigKind {
    pub fn all() -> &'static [RigKind] {
        &[
            #[cfg(feature = "ft817")]
            RigKind::Ft817,
        ]
    }
}

impl std::fmt::Display for RigKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "ft817")]
            RigKind::Ft817 => write!(f, "ft817"),
        }
    }
}

/// Connection details for instantiating a rig backend.
#[derive(Debug, Clone)]
pub enum RigAccess {
    Serial { path: String, baud: u32 },
    Tcp { addr: String },
}

/// Instantiate a rig backend based on the selected kind and access method.
pub fn build_rig(kind: RigKind, access: RigAccess) -> DynResult<Box<dyn RigCat>> {
    match (kind, access) {
        // Yaesu FT-817
        #[cfg(feature = "ft817")]
        (RigKind::Ft817, RigAccess::Serial { path, baud }) => {
            Ok(Box::new(Ft817::new(&path, baud)?))
        }
        #[cfg(feature = "ft817")]
        (RigKind::Ft817, RigAccess::Tcp { .. }) => {
            Err("FT-817 only supports serial CAT access".into())
        }

        // Fallback for unsupported combinations
        #[allow(unreachable_patterns)]
        _ => Err("Selected rig is not enabled/available".into()),
    }
}
