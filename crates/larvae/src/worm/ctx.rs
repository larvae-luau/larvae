/*!
The view a worm has of one file, and the edits the worm returns.

The context is owned and `'static`, because both guest forms must reach it
without a reference into the syntax tree. The epoch protects against stale
handles. A handle carries the epoch of the file it was created for. When
larvae moves to another file, an old handle fails with an error. Thus a stale
handle cannot read the nodes of the wrong file.
*/

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::rules::edits::Edit;

use super::nodes::NodeTable;

/// The counter only increases across the process, so no two files share an epoch
static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);

/// One file, as a worm sees it
pub struct FileCtx {
    /// The epoch identifies this file's traversal. Every handle check compares against it.
    pub epoch: u64,
    /// The flattened tree
    pub table: NodeTable,
    /// The source bytes that the node spans index into
    pub src: String,
    /// The path in the form the user knows, for diagnostics
    pub path: String,
    /// The resolved value of this rule, from the worm's declaration and the user's override
    pub value: Option<toml::Value>,
    /*
    Larvae defers these edits, in the same way as for the builtin rules. A worm
    does not see the output of another worm and does not mutate during a walk.
    Thus every edit refers to the original bytes, and larvae applies all edits
    in one sorted splice at the end.
    */
    edits: Mutex<Vec<Edit>>,
}

impl FileCtx {
    /// Take a new epoch. The caller builds the table and supplies it.
    pub fn new(table: NodeTable, src: String, path: String) -> Self {
        Self {
            epoch: NEXT_EPOCH.fetch_add(1, Ordering::Relaxed),
            table,
            src,
            path,
            value: None,
            edits: Mutex::new(Vec::new()),
        }
    }

    /// Attach the configured value of the rule that runs now
    pub fn with_value(mut self, value: Option<toml::Value>) -> Self {
        self.value = value;

        self
    }

    /// Queue a replacement for the byte range of `id`
    pub fn replace(&self, id: u32, text: String) -> Result<(), EditError> {
        let node = self.table.get(id).ok_or(EditError::NoSuchNode(id))?;

        self.edits
            .lock()
            .expect("edits mutex")
            .push((node.span.0, node.span.1, text));

        Ok(())
    }

    /*
    A removal keeps the newlines, the same as in the rest of the tool. The
    retain lines guarantee holds only if every transform keeps the line count.
    This function keeps the line count itself, so a worm does not have to.
    */
    pub fn remove(&self, id: u32) -> Result<(), EditError> {
        let node = self.table.get(id).ok_or(EditError::NoSuchNode(id))?;

        let span = &self.src[node.span.0 as usize..node.span.1 as usize];
        let newlines = "\n".repeat(span.matches('\n').count());

        self.edits
            .lock()
            .expect("edits mutex")
            .push((node.span.0, node.span.1, newlines));

        Ok(())
    }

    /// Return every edit the worm queued, in the order the worm queued them
    pub fn take_edits(&self) -> Vec<Edit> {
        std::mem::take(&mut self.edits.lock().expect("edits mutex"))
    }

    /// Refuse a handle that was created for a different file
    pub fn check(&self, epoch: u64) -> Result<(), EditError> {
        if epoch == self.epoch {
            Ok(())
        } else {
            Err(EditError::StaleHandle)
        }
    }
}

/// The reason larvae refused a request from a worm
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditError {
    /// The handle refers to a file that is no longer current
    StaleHandle,
    /// The id is not in the table of this file
    NoSuchNode(u32),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleHandle => {
                write!(f, "node handle outlived its file, it cannot be used here")
            }

            Self::NoSuchNode(id) => write!(f, "no node {id} in this file"),
        }
    }
}

impl std::error::Error for EditError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{lexer, parser};

    fn ctx(src: &str) -> FileCtx {
        let lexed = lexer::lex(src).unwrap();
        let chunk = parser::parse(src, &lexed.toks).unwrap();
        let toks = lexed.toks.clone();

        let bytes = move |span: crate::syntax::ast::TokSpan| -> (u32, u32) {
            if span.is_empty() {
                let at = toks
                    .get(span.start as usize)
                    .map(|t| t.start)
                    .unwrap_or_default();

                return (at, at);
            }

            (
                toks[span.start as usize].start,
                toks[span.end as usize - 1].end,
            )
        };

        FileCtx::new(
            NodeTable::build(&chunk, &bytes),
            src.to_owned(),
            "test.luau".into(),
        )
    }

    fn find(c: &FileCtx, kind: super::super::nodes::Kind) -> u32 {
        c.table
            .iter()
            .find(|(_, n)| n.kind == kind)
            .map(|(id, _)| id)
            .expect("node exists")
    }

    #[test]
    fn every_file_gets_its_own_epoch() {
        assert_ne!(ctx("local a = 1").epoch, ctx("local a = 1").epoch);
    }

    #[test]
    fn a_handle_from_another_file_is_refused() {
        let a = ctx("local a = 1");
        let b = ctx("local b = 2");

        assert_eq!(b.check(a.epoch), Err(EditError::StaleHandle));
        assert_eq!(b.check(b.epoch), Ok(()));
    }

    #[test]
    fn replace_queues_the_nodes_byte_range() {
        let c = ctx("local x = 1\n");
        let number = find(&c, super::super::nodes::Kind::Number);

        c.replace(number, "2".into()).unwrap();

        assert_eq!(c.take_edits(), [(10, 11, "2".to_owned())]);
    }

    /// A removal must not change the line count, because retain lines depends on it
    #[test]
    fn remove_leaves_the_newlines_behind() {
        let src = "local x = function()\n  return 1\nend\n";
        let c = ctx(src);
        let f = find(&c, super::super::nodes::Kind::FunctionExpr);

        c.remove(f).unwrap();

        let edits = c.take_edits();
        let (start, end, text) = &edits[0];

        assert_eq!(
            src[*start as usize..*end as usize].matches('\n').count(),
            text.matches('\n').count()
        );
    }

    #[test]
    fn an_unknown_node_is_an_error_rather_than_a_panic() {
        let c = ctx("local a = 1");

        assert_eq!(
            c.replace(9999, "x".into()),
            Err(EditError::NoSuchNode(9999))
        );
    }

    #[test]
    fn taking_edits_empties_the_queue() {
        let c = ctx("local x = 1\n");
        let n = find(&c, super::super::nodes::Kind::Number);

        c.replace(n, "2".into()).unwrap();

        assert_eq!(c.take_edits().len(), 1);
        assert!(c.take_edits().is_empty());
    }
}
