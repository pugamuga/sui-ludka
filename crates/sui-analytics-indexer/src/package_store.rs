// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

use move_core_types::account_address::AccountAddress;
use sui_package_resolver::{
    error::Error as PackageResolverError, make_package, Package, PackageStore, Result,
};
use sui_rest_api::Client;
use sui_types::base_types::{ObjectID, SequenceNumber};
use sui_types::object::Object;
use thiserror::Error;
use typed_store::rocks::{DBMap, MetricConf};
use typed_store::traits::TableSummary;
use typed_store::traits::TypedStoreDebug;
use typed_store::{Map, TypedStoreError};
use typed_store_derive::DBMapUtils;

const STORE: &str = "RocksDB";

#[derive(Error, Debug)]
pub enum Error {
    #[error("{0}")]
    TypedStore(#[from] TypedStoreError),
}

impl From<Error> for PackageResolverError {
    fn from(source: Error) -> Self {
        match source {
            Error::TypedStore(store_error) => Self::Store {
                store: STORE,
                source: Box::new(store_error),
            },
        }
    }
}

#[derive(DBMapUtils)]
pub struct PackageStoreTables {
    pub(crate) packages: DBMap<ObjectID, Object>,
}

impl PackageStoreTables {
    pub fn new(path: &Path) -> Arc<Self> {
        Arc::new(Self::open_tables_read_write(
            path.to_path_buf(),
            MetricConf::default(),
            None,
            None,
        ))
    }
    pub(crate) fn update(&self, package: &Object) -> Result<()> {
        let mut batch = self.packages.batch();
        batch
            .insert_batch(&self.packages, std::iter::once((package.id(), package)))
            .map_err(Error::TypedStore)?;
        batch.write().map_err(Error::TypedStore)?;
        Ok(())
    }
}

/// Store which keeps package objects in a local rocksdb store. It is expected that this store is
/// kept updated with latest version of package objects while iterating over checkpoints. If the
/// local db is missing (or gets deleted), packages are fetched from a full node and local store is
/// updated
pub struct LocalDBPackageStore {
    package_store_tables: Arc<PackageStoreTables>,
    fallback_client: Client,
}

impl LocalDBPackageStore {
    pub fn new(path: &Path, rest_url: &str) -> Self {
        let rest_api_url = format!("{}/rest", rest_url);
        Self {
            package_store_tables: PackageStoreTables::new(path),
            fallback_client: Client::new(rest_api_url),
        }
    }

    pub fn update(&self, object: &Object) -> Result<()> {
        let Some(_package) = object.data.try_as_package() else {
            return Ok(());
        };
        self.package_store_tables.update(object)?;
        Ok(())
    }

    pub async fn get(&self, id: AccountAddress) -> Result<Object> {
        let object = if let Some(object) = self
            .package_store_tables
            .packages
            .get(&ObjectID::from(id))
            .map_err(Error::TypedStore)?
        {
            object
        } else {
            let object = self
                .fallback_client
                .get_object(ObjectID::from(id))
                .await
                .map_err(|_| PackageResolverError::PackageNotFound(id))?;
            self.update(&object)?;
            object
        };
        Ok(object)
    }
}

#[async_trait]
impl PackageStore for LocalDBPackageStore {
    async fn version(&self, id: AccountAddress) -> Result<SequenceNumber> {
        let object = self.get(id).await?;
        Ok(object.version())
    }

    async fn fetch(&self, id: AccountAddress) -> Result<Arc<Package>> {
        let object = self.get(id).await?;
        let package = Arc::new(make_package(
            AccountAddress::from(object.id()),
            object.version(),
            &object,
        )?);
        Ok(package)
    }
}
