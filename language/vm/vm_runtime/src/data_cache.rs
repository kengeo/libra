// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0
//! Scratchpad for on chain values during the execution.

use libra_logger::prelude::*;
use libra_types::{
    access_path::AccessPath,
    language_storage::ModuleId,
    vm_error::{sub_status, StatusCode, VMStatus},
    write_set::{WriteOp, WriteSet, WriteSetMut},
};
use state_view::StateView;
use std::{collections::btree_map::BTreeMap, mem::replace};
use vm::{
    errors::*,
    gas_schedule::{AbstractMemorySize, GasAlgebra, GasCarrier},
};
use vm_runtime_types::{
    loaded_data::struct_def::StructDef,
    value::{GlobalRef, Struct, Value},
};

/// The wrapper around the StateVersionView for the block.
/// It keeps track of the value that have been changed during execution of a block.
/// It's effectively the write set for the block.
pub struct BlockDataCache<'block> {
    data_view: &'block dyn StateView,
    // TODO: an AccessPath corresponds to a top level resource but that may not be the
    // case moving forward, so we need to review this.
    // Also need to relate this to a ResourceKey.
    data_map: BTreeMap<AccessPath, Vec<u8>>,
}

impl<'block> BlockDataCache<'block> {
    pub fn new(data_view: &'block dyn StateView) -> Self {
        BlockDataCache {
            data_view,
            data_map: BTreeMap::new(),
        }
    }

    pub fn get(&self, access_path: &AccessPath) -> VMResult<Option<Vec<u8>>> {
        match self.data_map.get(access_path) {
            Some(data) => Ok(Some(data.clone())),
            None => match self.data_view.get(&access_path) {
                Ok(remote_data) => Ok(remote_data),
                // TODO: should we forward some error info?
                Err(_) => {
                    crit!("[VM] Error getting data from storage for {:?}", access_path);
                    Err(VMStatus::new(StatusCode::STORAGE_ERROR))
                }
            },
        }
    }

    pub fn push_write_set(&mut self, write_set: &WriteSet) {
        for (ref ap, ref write_op) in write_set.iter() {
            match write_op {
                WriteOp::Value(blob) => {
                    self.data_map.insert(ap.clone(), blob.clone());
                }
                WriteOp::Deletion => {
                    self.data_map.remove(ap);
                }
            }
        }
    }
}

/// Trait for the StateVersionView or a mock implementation of the remote cache.
/// Unit and integration tests should use this to mock implementations of "storage"
pub trait RemoteCache {
    fn get(&self, access_path: &AccessPath) -> VMResult<Option<Vec<u8>>>;
}

impl<'block> RemoteCache for BlockDataCache<'block> {
    fn get(&self, access_path: &AccessPath) -> VMResult<Option<Vec<u8>>> {
        BlockDataCache::get(self, access_path)
    }
}

/// Global cache for a transaction.
/// Materializes Values from the RemoteCache and keeps an Rc to them.
/// It also implements the opcodes that talk to storage and gives the proper guarantees of
/// reference lifetime.
/// Dirty objects are serialized and returned in make_write_set
pub struct TransactionDataCache<'txn> {
    // TODO: an AccessPath corresponds to a top level resource but that may not be the
    // case moving forward, so we need to review this.
    // Also need to relate this to a ResourceKey.
    data_map: BTreeMap<AccessPath, GlobalRef>,
    data_cache: &'txn dyn RemoteCache,
}

impl<'txn> TransactionDataCache<'txn> {
    pub fn new(data_cache: &'txn dyn RemoteCache) -> Self {
        TransactionDataCache {
            data_cache,
            data_map: BTreeMap::new(),
        }
    }

    // Retrieve data from the local cache or loads it from the remote cache into the local cache.
    // All operations on the global data are based on this API and they all load the data
    // into the cache.
    // TODO: this may not be the most efficient model because we always load data into the
    // cache even when that would not be strictly needed. Review once we have the whole story
    // working
    fn load_data(&mut self, ap: &AccessPath, def: StructDef) -> VMResult<&mut GlobalRef> {
        if !self.data_map.contains_key(ap) {
            match self.data_cache.get(ap)? {
                Some(bytes) => {
                    let res = Value::simple_deserialize(&bytes, def)?;
                    let new_root = GlobalRef::make_root(ap.clone(), res);
                    self.data_map.insert(ap.clone(), new_root);
                }
                None => {
                    return Err(vm_error(Location::new(), StatusCode::MISSING_DATA));
                }
            };
        }
        Ok(self.data_map.get_mut(ap).expect("data must exist"))
    }

    /// BorrowGlobal opcode cache implementation
    pub fn borrow_global(&mut self, ap: &AccessPath, def: StructDef) -> VMResult<GlobalRef> {
        let root_ref = match self.load_data(ap, def) {
            Ok(gref) => gref,
            Err(e) => {
                error!("[VM] (BorrowGlobal) Error reading data for {}: {:?}", ap, e);
                return Err(e);
            }
        };
        // is_loadable() checks ref count and whether the data was deleted
        if root_ref.is_loadable() {
            // shallow_ref increment ref count
            Ok(root_ref.clone())
        } else {
            Err(
                vm_error(Location::new(), StatusCode::DYNAMIC_REFERENCE_ERROR)
                    .with_sub_status(sub_status::DRE_GLOBAL_ALREADY_BORROWED),
            )
        }
    }

    /// Exists opcode cache implementation
    pub fn resource_exists(
        &mut self,
        ap: &AccessPath,
        def: StructDef,
    ) -> VMResult<(bool, AbstractMemorySize<GasCarrier>)> {
        Ok(match self.load_data(ap, def) {
            Ok(gref) => {
                if gref.is_deleted() {
                    (false, AbstractMemorySize::new(0))
                } else {
                    (true, gref.size())
                }
            }
            Err(_) => (false, AbstractMemorySize::new(0)),
        })
    }

    /// MoveFrom opcode cache implementation
    pub fn move_resource_from(&mut self, ap: &AccessPath, def: StructDef) -> VMResult<Value> {
        let root_ref = match self.load_data(ap, def) {
            Ok(gref) => gref,
            Err(e) => {
                warn!("[VM] (MoveFrom) Error reading data for {}: {:?}", ap, e);
                return Err(e);
            }
        };
        // is_loadable() checks ref count and whether the data was deleted
        if root_ref.is_loadable() {
            Ok(root_ref.move_from()?)
        } else {
            Err(
                vm_error(Location::new(), StatusCode::DYNAMIC_REFERENCE_ERROR)
                    .with_sub_status(sub_status::DRE_GLOBAL_ALREADY_BORROWED),
            )
        }
    }

    /// MoveToSender opcode cache implementation
    pub fn move_resource_to(
        &mut self,
        ap: &AccessPath,
        def: StructDef,
        res: Struct,
    ) -> VMResult<()> {
        // a resource can be written to an AccessPath if the data does not exists or
        // it was deleted (MoveFrom)
        let can_write = match self.load_data(ap, def) {
            Ok(data) => data.is_deleted(),
            Err(e) => match e.major_status {
                StatusCode::MISSING_DATA => true,
                _ => return Err(e),
            },
        };
        if can_write {
            let new_root = GlobalRef::move_to(ap.clone(), res);
            self.data_map.insert(ap.clone(), new_root);
            Ok(())
        } else {
            warn!("[VM] Cannot write over existing resource {}", ap);
            Err(vm_error(
                Location::new(),
                StatusCode::CANNOT_WRITE_EXISTING_RESOURCE,
            ))
        }
    }

    /// Make a write set from the updated (dirty, deleted) global resources along with
    /// to-be-published modules.
    /// Consume the TransactionDataCache and must be called at the end of a transaction.
    /// This also ends up checking that reference count around global resources is correct
    /// at the end of the transactions (all ReleaseRef are properly called)
    pub fn make_write_set(
        &mut self,
        to_be_published_modules: Vec<(ModuleId, Vec<u8>)>,
    ) -> VMResult<WriteSet> {
        let mut write_set = WriteSetMut::new(Vec::new());
        let data_map = replace(&mut self.data_map, BTreeMap::new());
        for (key, global_ref) in data_map {
            if !global_ref.is_clean() {
                // if there are pending references get_data() returns None
                // this is the check at the end of a transaction to verify all references
                // are properly released
                let deleted = global_ref.is_deleted();
                if let Some(data) = global_ref.get_data() {
                    if deleted {
                        // The write set will not grow to this size due to the gas limit.
                        // Expressing the bound in terms of the gas limit is impractical
                        // for MIRAI to check to we set a safe upper bound.
                        assume!(write_set.len() < usize::max_value());
                        write_set.push((key, WriteOp::Deletion));
                    } else if let Some(blob) = data.simple_serialize() {
                        // The write set will not grow to this size due to the gas limit.
                        // Expressing the bound in terms of the gas limit is impractical
                        // for MIRAI to check to we set a safe upper bound.
                        assume!(write_set.len() < usize::max_value());
                        write_set.push((key, WriteOp::Value(blob)));
                    } else {
                        return Err(vm_error(
                            Location::new(),
                            StatusCode::VALUE_SERIALIZATION_ERROR,
                        ));
                    }
                } else {
                    return Err(
                        vm_error(Location::new(), StatusCode::DYNAMIC_REFERENCE_ERROR)
                            .with_sub_status(sub_status::DRE_MISSING_RELEASEREF),
                    );
                }
            }
        }

        // Insert the code blob to the writeset.
        if write_set.len() <= usize::max_value() - to_be_published_modules.len() {
            for (key, blob) in to_be_published_modules.into_iter() {
                write_set.push(((&key).into(), WriteOp::Value(blob)));
            }
        } else {
            return Err(vm_error(Location::new(), StatusCode::INVALID_DATA));
        }

        write_set
            .freeze()
            .map_err(|_| vm_error(Location::new(), StatusCode::DATA_FORMAT_ERROR))
    }

    /// Flush out the cache and restart from a clean state
    pub fn clear(&mut self) {
        self.data_map.clear()
    }
}
