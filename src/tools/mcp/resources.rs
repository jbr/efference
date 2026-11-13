//! A connected provider's resources (docs/tools.md "Resources"), read through one tool per
//! server that offers them: `resources__<server>`, a listing without `uri` and a read with one.
//!
//! The tool sits in efference's own `resources` namespace rather than the server's. A server
//! names its own tools, `resources` included, but no declared server may take an efference
//! namespace, so no connected tool can be named `resources__<anything>`.

use std::collections::HashSet;
use std::future::Future;

use mcplease::client::Paginated;
use mcplease::types::{
    CallToolResult, ContentBlock, JsonRpcError, ListResourceTemplatesResult, ListResourcesResult,
    Outcome, ReadResourceResult, Resource, ResourceTemplate, Tool as ToolDefinition,
    ToolAnnotations, error_codes,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{
    Abort, ConnectedProvider, ServerEvent, Session, describe_input_required, render_result,
};
use crate::record::log::{Notification, RawResult};
use crate::record::names::tools::{NAMESPACE_SEPARATOR, RESOURCES_NAMESPACE};
use crate::tools::Interrupt;

/// The wire name of `server`'s resource tool.
pub(super) fn wire_name(server: &str) -> String {
    format!("{RESOURCES_NAMESPACE}{NAMESPACE_SEPARATOR}{server}")
}

/// The tool definition mounted for a server that negotiated the `resources` capability.
pub(super) fn definition(server: &str) -> ToolDefinition {
    let mut tool = ToolDefinition::new(
        wire_name(server),
        json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "The resource to read, as the listing gives it or a URI \
                        template with its {…} variables filled in. Omit to list."
                }
            }
        }),
    );
    tool.description = Some(format!(
        "Read the resources the `{server}` MCP server offers. Call without `uri` to list its \
         resources and URI templates; call with a `uri` to read that resource."
    ));
    tool.annotations = Some(ToolAnnotations {
        read_only_hint: Some(true),
        ..ToolAnnotations::default()
    });
    tool
}

impl ConnectedProvider {
    /// Run `resources__<server>`: list without a `uri`, read with one. Every outcome lands as
    /// a result, so the `tool_use` always gets its pair.
    pub(super) async fn resources(
        &self,
        input: &Value,
        abort: impl Future<Output = Interrupt> + Send,
        on_output: impl FnMut(&[u8]) + Send,
        on_notify: impl FnMut(Notification) + Send,
    ) -> RawResult {
        let uri = match input.get("uri") {
            None | Some(Value::Null) => None,
            Some(Value::String(uri)) if uri.trim().is_empty() => None,
            Some(Value::String(uri)) => Some(uri.trim()),
            Some(other) => {
                return RawResult::error(format!("`uri` must be a string, not {other}"));
            }
        };
        let mut session = self.session.lock().await;
        let abort = std::pin::pin!(abort);
        let mut abort = Abort::new(abort);
        let mut on_event = self.event_sink(on_output, on_notify);
        let call = Call {
            server: &self.name,
            session: &mut session,
            abort: &mut abort,
            on_event: &mut on_event,
        };
        match uri {
            None => call.list().await,
            Some(uri) => call.read(uri).await,
        }
    }
}

/// One `resources__<server>` call in flight: the session it holds and the interrupt it races.
struct Call<'c, 'a, A, E> {
    server: &'c str,
    session: &'c mut Session,
    abort: &'c mut Abort<'a, A>,
    on_event: &'c mut E,
}

impl<A, E> Call<'_, '_, A, E>
where
    A: Future<Output = Interrupt> + Send,
    E: FnMut(ServerEvent) + Send,
{
    async fn list(mut self) -> RawResult {
        let resources = match self.drain::<ListResourcesResult>().await {
            Ok(resources) => resources,
            Err(result) => return result,
        };
        let templates = match self.drain::<ListResourceTemplatesResult>().await {
            Ok(templates) => templates,
            Err(result) => return result,
        };
        RawResult::text(render_listing(self.server, &resources, &templates))
    }

    /// Every page of one listing. A cursor the server already issued ends it with an error
    /// rather than looping: nothing else bounds a call but the operator's interrupt.
    async fn drain<P: Paginated + DeserializeOwned>(&mut self) -> Result<Vec<P::Item>, RawResult> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        loop {
            let request = self.session.protocol.page::<P>(cursor.as_deref());
            let page = match self.exchange(request).await? {
                Ok(page) => serde_json::from_value::<P>(page).map_err(anyhow::Error::from),
                Err(e) => Err(e),
            };
            let page = page.map_err(|e| {
                RawResult::error(format!("`{}` failed {}: {e}", self.server, P::METHOD))
            })?;
            cursor = page.next_cursor().map(String::from);
            items.extend(page.into_items());
            match &cursor {
                None => return Ok(items),
                Some(next) if !seen.insert(next.clone()) => {
                    return Err(RawResult::error(format!(
                        "`{}` repeated the {} cursor {next:?}; the listing would never end",
                        self.server,
                        P::METHOD
                    )));
                }
                Some(_) => {}
            }
        }
    }

    async fn read(mut self, uri: &str) -> RawResult {
        let request = self.session.protocol.resources_read_with_progress(uri);
        let server = self.server;
        let read = match self.exchange(request).await {
            Ok(Ok(read)) => read,
            Ok(Err(e)) if is_not_found(&e) => {
                return RawResult::error(format!(
                    "`{server}` has no resource at {uri}: {e}. Call `{}` without `uri` to list \
                     what it serves.",
                    wire_name(server)
                ));
            }
            Ok(Err(e)) => {
                return RawResult::error(format!("reading {uri} from `{server}` failed: {e}"));
            }
            Err(result) => return result,
        };
        match serde_json::from_value::<Outcome<ReadResourceResult>>(read) {
            // Not a missing resource: the spec forbids answering one with empty contents.
            Ok(Outcome::Complete(read)) if read.contents.is_empty() => {
                RawResult::text(format!("{uri} is empty"))
            }
            // Rendered as embedded resources are in a tool result, so a resource reads the
            // same whichever way it arrived.
            Ok(Outcome::Complete(read)) => render_result(&CallToolResult {
                content: read
                    .contents
                    .into_iter()
                    .map(|resource| ContentBlock::EmbeddedResource {
                        resource,
                        annotations: None,
                        meta: None,
                    })
                    .collect(),
                ..CallToolResult::default()
            }),
            Ok(Outcome::InputRequired(interim)) => RawResult::error(format!(
                "`{server}` answered the read of {uri} asking for more input ({}), which \
                 efference does not supply",
                describe_input_required(&interim),
            )),
            Err(e) => RawResult::error(format!("`{server}` returned an unparseable resource: {e}")),
        }
    }

    /// One request, raced against the interrupt. `Err` is the abandoned call's paired result.
    async fn exchange(
        &mut self,
        request: mcplease::types::JsonRpcRequest,
    ) -> Result<anyhow::Result<Value>, RawResult> {
        self.session
            .exchange(request, self.abort, self.on_event)
            .await
            .ok_or_else(|| {
                RawResult::error(format!(
                    "`{}` call cancelled by the operator",
                    wire_name(self.server)
                ))
            })
    }
}

/// Whether a failed read's error code is a not-found code: invalid params on this revision,
/// or the code earlier revisions used, which the spec asks clients to accept too.
fn is_not_found(error: &anyhow::Error) -> bool {
    error.downcast_ref::<JsonRpcError>().is_some_and(|e| {
        matches!(
            e.code,
            error_codes::INVALID_PARAMS | error_codes::RESOURCE_NOT_FOUND
        )
    })
}

/// A listing the model reads: each resource's URI first, since that is what it passes back,
/// then what the server calls it and says about it.
fn render_listing(server: &str, resources: &[Resource], templates: &[ResourceTemplate]) -> String {
    if resources.is_empty() && templates.is_empty() {
        return format!("`{server}` lists no resources or URI templates.");
    }
    let mut text = String::new();
    if !resources.is_empty() {
        text.push_str(&format!("Resources from `{server}`:\n"));
        for resource in resources {
            entry(
                &mut text,
                &resource.uri,
                resource.title.as_deref().unwrap_or(&resource.name),
                resource.mime_type.as_deref(),
                resource.description.as_deref(),
            );
        }
    }
    if !templates.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!(
            "URI templates from `{server}` — fill in the {{…}} variables, then read the URI:\n"
        ));
        for template in templates {
            entry(
                &mut text,
                &template.uri_template,
                template.title.as_deref().unwrap_or(&template.name),
                template.mime_type.as_deref(),
                template.description.as_deref(),
            );
        }
    }
    text.truncate(text.trim_end().len());
    text
}

fn entry(text: &mut String, uri: &str, label: &str, mime: Option<&str>, description: Option<&str>) {
    text.push_str(&format!("- {uri} — {label}"));
    if let Some(mime) = mime {
        text.push_str(&format!(" ({mime})"));
    }
    text.push('\n');
    if let Some(description) = description.map(str::trim).filter(|d| !d.is_empty()) {
        text.push_str(&format!("  {description}\n"));
    }
}
