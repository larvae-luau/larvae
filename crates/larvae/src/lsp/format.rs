/*!
`textDocument/formatting`: one edit that replaces the whole document.
*/

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::fmt;
use crate::worm::proto;

use super::uri::path_of_uri;
use super::{Server, rpc};

impl Server {
    /// Reports if the `[lsp]` mode leaves this file to another server
    pub(super) fn declines(&self, uri: &str) -> bool {
        if !self.lsp.enabled {
            return true;
        }

        self.lsp.claim_only
            && !path_of_uri(uri).is_some_and(|p| self.worms.frontend_for(&p).is_some())
    }

    pub(super) fn format(&self, uri: &str) -> Result<Value> {
        if self.declines(uri) {
            return Ok(Value::Null);
        }

        let Some(src) = self.documents.get(uri) else {
            return Ok(Value::Null);
        };

        // `[fmt] enabled = false` reaches the editor as a formatter with no edits
        if !self.fmt.enabled {
            return Ok(json!([]));
        }

        let Some(formatted) = self.formatted(uri, src)? else {
            return Ok(json!([]));
        };

        // An edit that changes nothing still makes the editor mark the file dirty.
        if formatted == *src {
            return Ok(json!([]));
        }

        Ok(json!([{
            "range": rpc::Lines::new(src).whole(src),
            "newText": formatted,
        }]))
    }

    /*
    The formatted text of one document, from the owner of its extension.

    A claimed file goes to its worm. The worm replies with a layout document,
    and larvae renders it in the style of the project. A worm that does not
    format its files gives `None` here, and the server then sends no edit. A
    message is correct for `larvae fmt`, because a user named that file. A
    message is wrong for an editor, because the editor asks on each save.
    */
    fn formatted(&self, uri: &str, src: &str) -> Result<Option<String>> {
        let Some(index) = path_of_uri(uri).and_then(|p| self.worms.frontend_for(&p)) else {
            return fmt::format(src, &self.fmt).map(Some);
        };

        let spec = self.worms.spec(index);

        if !spec.formats() {
            return Ok(None);
        }

        let reply = self.worms.format(index, src)?;

        // a project can keep one option out of the files this worm claims
        let cfg = self.fmt.without(&spec.inherit.fmt_except);

        proto::render_format(src, &reply, &cfg)
            .with_context(|| format!("worm `{}`", spec.manifest.name))
            .map(Some)
    }
}
