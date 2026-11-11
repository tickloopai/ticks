//! What the Browser Tick declares: the extension's tools while it is connected, and only the `status`
//! query until then. The revision is a digest of the tools, so the same catalog keeps its revision
//! across reconnections and any change makes the host prepare its calls again.

use std::sync::{Arc, Mutex};

use serde_json::json;
use sha1::{Digest, Sha1};
use tick_protocol::{ScreenDeclaration, TickDescriptor, TickTool};
use tick_sdk::stream::Describe;

pub const KIND: &str = "browser";
const TITLE: &str = "Browser";
const DESCRIPTION: &str = "Read and operate the Agent's tab group in the user's own Chrome, Chromium or Edge through the Tickloop browser extension.";

/// The query tool the Tick answers itself.
pub const STATUS_TOOL: &str = "status";

/// The fewest rows the card stays usable at, its frame included.
pub const MIN_ROWS: u16 = 6;
/// The most rows the card puts to use, for the list of tabs. The card asks for fewer while there
/// is less to show (`view::wanted_rows`), so a host that follows it shrinks the card back.
pub const MAX_ROWS: u16 = 12;

/// The catalog the Tick serves now, shared with the stream that answers `tick/describe`.
#[derive(Clone, Default)]
pub struct Catalog(Arc<Mutex<Option<Vec<TickTool>>>>);

impl Catalog {
    fn tools(&self) -> std::sync::MutexGuard<'_, Option<Vec<TickTool>>> {
        self.0.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Serves the extension's `tools` from now on, or says why they cannot be: the same checks the
    /// host makes of a description, so an accepted catalog never fails `tick/describe`.
    pub fn serve(&self, tools: Vec<TickTool>) -> Result<(), String> {
        check(&tools)?;
        *self.tools() = Some(tools);
        Ok(())
    }

    /// Goes back to the `status` query alone, as when the extension disconnects.
    pub fn withdraw(&self) {
        *self.tools() = None;
    }

    /// Whether the extension's tools are served, rather than the `status` query.
    pub fn serves_extension(&self) -> bool {
        self.tools().is_some()
    }

    /// The extension's tool `name`, while the extension's tools are served.
    pub fn extension_tool(&self, name: &str) -> Option<TickTool> {
        self.tools()
            .as_ref()?
            .iter()
            .find(|tool| tool.name == name)
            .cloned()
    }

    pub fn descriptor(&self) -> TickDescriptor {
        descriptor(self.tools().clone().unwrap_or_else(|| vec![status_tool()]))
    }
}

/// Whether the extension's `tools` can be served: the checks the host makes of a description. The
/// native host makes them of the extension's `hello`, and each Tick again of what it serves.
pub fn check(tools: &[TickTool]) -> Result<(), String> {
    let descriptor = descriptor(tools.to_vec());
    descriptor.validate()?;
    tick_protocol::check_encoded_size(
        &descriptor,
        tick_protocol::MAX_DESCRIPTION_BYTES,
        "the extension's catalog",
    )
}

impl Describe for Catalog {
    fn descriptor(&self) -> Result<TickDescriptor, String> {
        Ok(Catalog::descriptor(self))
    }
}

fn descriptor(tools: Vec<TickTool>) -> TickDescriptor {
    TickDescriptor {
        protocol: tick_protocol::NATIVE_PROTOCOL_VERSION,
        kind: KIND.to_owned(),
        title: TITLE.to_owned(),
        description: DESCRIPTION.to_owned(),
        revision: revision(&tools),
        tools,
        recover_with: None,
        state: None,
        screen: Some(ScreenDeclaration {
            min_rows: Some(MIN_ROWS),
            max_rows: Some(MAX_ROWS),
        }),
    }
}

/// A digest of the tools as declared, in their order.
fn revision(tools: &[TickTool]) -> String {
    let digest = Sha1::digest(serde_json::to_vec(tools).expect("tools serialize"));
    format!("{digest:x}")[..16].to_owned()
}

/// The query the Tick answers while no extension is connected.
pub fn status_tool() -> TickTool {
    TickTool {
        name: STATUS_TOOL.to_owned(),
        description: "Report whether the browser extension is connected, the local socket it connects through and where the native messaging host is registered for each browser. The browser's tools appear once the extension connects.".to_owned(),
        input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        mutates: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, mutates: bool) -> TickTool {
        TickTool {
            name: name.to_owned(),
            description: format!("{name} in the Agent's tab."),
            input_schema: json!({"type": "object", "properties": {"tab_id": {"type": "integer"}}}),
            mutates,
        }
    }

    #[test]
    fn the_catalog_follows_the_extension_and_its_revision_follows_the_tools() {
        let catalog = Catalog::default();
        let waiting = catalog.descriptor();
        waiting.validate().unwrap();
        assert_eq!(waiting.kind, "browser");
        assert_eq!(waiting.tools, [status_tool()]);
        assert_eq!(
            waiting.screen,
            Some(ScreenDeclaration {
                min_rows: Some(6),
                max_rows: Some(12)
            })
        );

        let tools = vec![tool("read_page", false), tool("computer", true)];
        catalog.serve(tools.clone()).unwrap();
        let connected = catalog.descriptor();
        assert_eq!(connected.tools, tools);
        assert_ne!(connected.revision, waiting.revision);
        assert!(catalog.extension_tool("computer").unwrap().mutates);
        assert!(catalog.extension_tool(STATUS_TOOL).is_none());

        catalog.withdraw();
        assert_eq!(catalog.descriptor(), waiting);
        assert!(catalog.extension_tool("computer").is_none());

        // Reconnecting with the same tools keeps the revision the host approved calls against.
        catalog.serve(tools.clone()).unwrap();
        assert_eq!(catalog.descriptor().revision, connected.revision);
        // A tool that changes meaning changes it.
        catalog
            .serve(vec![tool("read_page", false), tool("computer", false)])
            .unwrap();
        assert_ne!(catalog.descriptor().revision, connected.revision);
    }

    #[test]
    fn a_catalog_the_host_would_reject_is_not_served() {
        let catalog = Catalog::default();
        catalog.serve(vec![tool("tabs", false)]).unwrap();
        let served = catalog.descriptor();
        for tools in [
            vec![tool("tabs", false), tool("tabs", true)],
            vec![tool(" ", false)],
            vec![TickTool {
                input_schema: json!("not a schema"),
                ..tool("tabs", false)
            }],
            vec![TickTool {
                description: "x".repeat(tick_protocol::MAX_DESCRIPTION_BYTES),
                ..tool("tabs", false)
            }],
        ] {
            assert!(catalog.serve(tools.clone()).is_err(), "{tools:?}");
            assert_eq!(catalog.descriptor(), served);
        }
    }
}
