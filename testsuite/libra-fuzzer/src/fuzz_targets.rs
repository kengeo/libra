// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{FuzzTarget, FuzzTargetImpl};
use failure::prelude::*;
use lazy_static::lazy_static;
use std::{collections::BTreeMap, env};

/// Convenience macro to return the module name.
macro_rules! module_name {
    () => {
        module_path!()
            .rsplit("::")
            .next()
            .expect("module path must have at least one component")
    };
}

/// A fuzz target implementation for protobuf-compiled targets.
macro_rules! proto_fuzz_target {
    ($target:ident => $ty:ty, $prototy:ty) => {
        #[derive(Clone, Debug, Default)]
        pub struct $target;

        impl $crate::FuzzTargetImpl for $target {
            fn name(&self) -> &'static str {
                module_name!()
            }

            fn description(&self) -> &'static str {
                concat!(stringify!($ty), " (protobuf)")
            }

            fn generate(
                &self,
                _idx: usize,
                gen: &mut ::proptest_helpers::ValueGenerator,
            ) -> Option<Vec<u8>> {
                use prost_ext::MessageExt;

                let value: $prototy = gen.generate(::proptest::arbitrary::any::<$ty>()).into();

                Some(value.to_vec().expect("failed to convert to bytes"))
            }

            fn fuzz(&self, data: &[u8]) {
                use prost::Message;
                use std::convert::TryFrom;

                // Errors are OK -- the fuzzer cares about panics and OOMs.
                let _ = <$prototy>::decode(data).map(<$ty>::try_from);
            }
        }
    };
}

// List fuzz target modules here.
mod admission_control;
mod compiled_module;
mod consensus_proposal;
mod inbound_rpc_protocol;
mod inner_signed_transaction;
mod signed_transaction;
mod vm_value;

lazy_static! {
    static ref ALL_TARGETS: BTreeMap<&'static str, Box<dyn FuzzTargetImpl>> = {
        let targets: Vec<Box<dyn FuzzTargetImpl>> = vec![
            // List fuzz targets here in this format.
            Box::new(compiled_module::CompiledModuleTarget::default()),
            Box::new(signed_transaction::SignedTransactionTarget::default()),
            Box::new(inner_signed_transaction::SignedTransactionTarget::default()),
            Box::new(vm_value::ValueTarget::default()),
            Box::new(consensus_proposal::ConsensusProposal::default()),
            Box::new(admission_control::AdmissionControlSubmitTransactionRequest::default()),
            Box::new(inbound_rpc_protocol::RpcInboundRequest::default()),
        ];
        targets.into_iter().map(|target| (target.name(), target)).collect()
    };
}

impl FuzzTarget {
    /// The environment variable used for passing fuzz targets to child processes.
    pub(crate) const ENV_VAR: &'static str = "FUZZ_TARGET";

    /// Get the current fuzz target from the environment.
    pub fn from_env() -> Result<Self> {
        let name = env::var(Self::ENV_VAR)?;
        Self::by_name(&name).ok_or_else(|| format_err!("Unknown fuzz target '{}'", name))
    }

    /// Get a fuzz target by name.
    pub fn by_name(name: &str) -> Option<Self> {
        ALL_TARGETS.get(name).map(|target| FuzzTarget(&**target))
    }

    /// A list of all fuzz targets.
    pub fn all_targets() -> impl Iterator<Item = Self> {
        ALL_TARGETS.values().map(|target| FuzzTarget(&**target))
    }
}
