//! MCP model discovery backed by the host catalogue (docs/model-catalog.md).

use super::{Control, Executor};
use crate::inference::catalog::resource::{Query, URI};
use mcplease::{Resources, types::*};

/// The model catalogue and its filterable URI template.
pub struct ModelResources;

const DESCRIPTION: &str = "Configured model slugs, capabilities and USD-per-million-token prices; unreported prices are unknown. Ranked by system-wide decayed model-pick frequencies. Optional query parameters combine: connection (exact name), q (case-insensitive ID/name/description substring), role (main or triage exclude decision-only models; spotter includes them), limit (1–100, default 20), offset (default 0). Role is compatibility, not task advice. Cached listings refresh in the background; read again for updates.";

fn discovery_params(params: &PaginatedRequestParams) -> Result<(), JsonRpcError> {
    if params.cursor.is_some() {
        return Err(JsonRpcError::invalid_params(
            "resource discovery has no further pages",
        ));
    }
    Ok(())
}

impl Resources<Control> for ModelResources {
    async fn list(
        _state: Control,
        params: PaginatedRequestParams,
        _context: RequestContext,
    ) -> Result<ListResourcesResult, JsonRpcError> {
        discovery_params(&params)?;
        Ok(ListResourcesResult {
            resources: vec![Resource {
                name: "models".into(),
                uri: URI.into(),
                title: Some("Available models".into()),
                description: Some(DESCRIPTION.into()),
                mime_type: Some("text/plain".into()),
                ..Resource::default()
            }],
            ..ListResourcesResult::default()
        })
    }

    async fn list_templates(
        _state: Control,
        params: PaginatedRequestParams,
        _context: RequestContext,
    ) -> Result<ListResourceTemplatesResult, JsonRpcError> {
        discovery_params(&params)?;
        Ok(ListResourceTemplatesResult {
            resource_templates: vec![ResourceTemplate {
                name: "models-query".into(),
                uri_template: format!("{URI}{{?connection,q,role,limit,offset}}"),
                title: Some("Search available models".into()),
                description: Some(DESCRIPTION.into()),
                mime_type: Some("text/plain".into()),
                ..ResourceTemplate::default()
            }],
            ..ListResourceTemplatesResult::default()
        })
    }

    async fn read(
        state: Control,
        params: ReadResourceRequestParams,
        _context: RequestContext,
    ) -> Result<ReadResourceResult, JsonRpcError> {
        if !Query::addresses(&params.uri) {
            return Err(JsonRpcError::invalid_params(format!(
                "no model resource at {:?}",
                params.uri
            )));
        }
        let query = match Query::parse(&params.uri) {
            Ok(query) => query,
            Err(error) => {
                return Ok(text_result(
                    params.uri,
                    format!("Invalid model query: {error}"),
                ));
            }
        };
        let connections = match state.executor {
            Executor::Client(executor) => executor
                .host_client()
                .map_err(|e| JsonRpcError::internal(e.to_string()))?
                .catalog_global()
                .await
                .map_err(|e| JsonRpcError::internal(e.to_string()))?,
            Executor::Host(executor) => {
                let host = executor
                    .host()
                    .map_err(|e| JsonRpcError::internal(e.to_string()))?;
                let picks = host.catalog_recents(None);
                let mut connections = Vec::new();
                for row in host.inference.inventory() {
                    connections.push(host.catalog_entry(&row, &picks, None).await);
                }
                connections
            }
        };
        let text = match query.select(connections) {
            Ok(connections) => query.render(&connections),
            Err(error) => format!("Invalid model query: {error}"),
        };
        Ok(text_result(params.uri, text))
    }
}

fn text_result(uri: String, text: String) -> ReadResourceResult {
    ReadResourceResult {
        contents: vec![ResourceContents::Text {
            uri,
            mime_type: Some("text/plain".into()),
            text,
        }],
        ..ReadResourceResult::default()
    }
}

#[cfg(test)]
mod test;
