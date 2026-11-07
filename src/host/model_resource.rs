//! The host-backed models resource (docs/model-catalog.md).
use super::Host;
use crate::config::history::Scope;
use crate::inference::catalog::resource::Query;
use crate::tools::resources::ResourceReader;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Weak},
};

struct Models {
    host: Weak<Host>,
    scope: Scope,
}

impl Host {
    /// A reader for this project's default-config recent picks. The inventory and
    /// refresh locks stay host-owned; the toolbox holds only a weak reference.
    pub(crate) fn model_resource_reader(
        self: &Arc<Self>,
        cwd: &std::path::Path,
    ) -> Box<dyn ResourceReader> {
        let project = crate::record::session_dir::worktree::project_of(cwd)
            .unwrap_or_else(|| cwd.to_path_buf());
        Box::new(Models {
            host: Arc::downgrade(self),
            scope: Scope::for_request(&project, None),
        })
    }
}

impl ResourceReader for Models {
    fn read<'a>(
        &'a self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            let query = Query::parse(uri)?;
            let host = self
                .host
                .upgrade()
                .ok_or("the model catalog's host has shut down")?;
            let picks = host.catalog_recents(Some(&self.scope));
            let mut connections = Vec::new();
            for row in host.inference.inventory() {
                connections.push(host.catalog_entry(&row, &picks, None).await);
            }
            Ok(query.render(&query.select(connections)?))
        })
    }
}
