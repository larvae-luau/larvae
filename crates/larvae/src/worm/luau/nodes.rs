/*!
The AST node a Luau worm holds, and the edits it makes through one.

A rule worm receives a node as an id and a kind and a byte span, never a tree,
because the batched protocol crosses one time per file. `NodeRef` is that
handle on the Luau side: it reads the text of its span and it asks the host to
replace or remove it.

A handle from an earlier file is refused. The pool keeps a worm instance per
worker and reuses it, so a worm that stored a node in an upvalue would
otherwise edit the file in work using a span from the file before it.
*/

use std::sync::Arc;

use super::*;

/*
A handle is two integers and nothing else. By intent, it does not hold the
file it came from. If a worm stores a handle in a global and uses it on the
next file, the epoch check catches this. Thus the handle does not read the
wrong tree silently.
*/
#[derive(Clone, Copy)]
pub(super) struct NodeRef {
    epoch: u64,
    id: u32,
}

impl NodeRef {
    pub(super) fn new(epoch: u64, id: u32) -> Self {
        Self { epoch, id }
    }
}

// This impl lets a function take a NodeRef argument and not an AnyUserData
impl mlua::FromLua for NodeRef {
    fn from_lua(value: mlua::Value, _: &mlua::Lua) -> mlua::Result<Self> {
        match value {
            mlua::Value::UserData(ud) => Ok(*ud.borrow::<NodeRef>()?),

            other => Err(mlua::Error::runtime(format!(
                "expected a node, got {}",
                other.type_name()
            ))),
        }
    }
}

/// The file that larvae walks now, or an error if a worm calls this outside a walk
pub(super) fn current(lua: &mlua::Lua) -> mlua::Result<Arc<FileCtx>> {
    lua.app_data_ref::<Arc<FileCtx>>()
        .map(|file| Arc::clone(&file))
        .ok_or_else(|| mlua::Error::runtime("no file is being walked right now"))
}

pub(super) fn checked(lua: &mlua::Lua, node: &NodeRef) -> mlua::Result<Arc<FileCtx>> {
    let file = current(lua)?;

    file.check(node.epoch)
        .map_err(|e| mlua::Error::runtime(e.to_string()))?;

    Ok(file)
}

impl mlua::UserData for NodeRef {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("kind", |lua, this, ()| {
            let file = checked(lua, this)?;
            let node = file
                .table
                .get(this.id)
                .ok_or_else(|| mlua::Error::runtime("no such node"))?;

            Ok(node.kind.name())
        });

        methods.add_method("text", |lua, this, ()| {
            let file = checked(lua, this)?;

            Ok(file.table.text(this.id, &file.src).unwrap_or("").to_owned())
        });

        // byte offsets into the original source, as a half open range
        methods.add_method("span", |lua, this, ()| {
            let file = checked(lua, this)?;
            let node = file
                .table
                .get(this.id)
                .ok_or_else(|| mlua::Error::runtime("no such node"))?;

            Ok((node.span.0, node.span.1))
        });

        methods.add_method("children", |lua, this, ()| {
            let file = checked(lua, this)?;
            let node = file
                .table
                .get(this.id)
                .ok_or_else(|| mlua::Error::runtime("no such node"))?;

            let out = lua.create_table()?;

            for (i, &child) in node.children.iter().enumerate() {
                out.set(i + 1, NodeRef::new(this.epoch, child))?;
            }

            Ok(out)
        });

        methods.add_method("parent", |lua, this, ()| {
            let file = checked(lua, this)?;
            let node = file
                .table
                .get(this.id)
                .ok_or_else(|| mlua::Error::runtime("no such node"))?;

            Ok(node.parent.map(|p| NodeRef::new(this.epoch, p)))
        });
    }
}

pub(super) fn edit_replace(
    lua: &mlua::Lua,
    (_ctx, node, text): (mlua::Value, NodeRef, String),
) -> mlua::Result<()> {
    let file = checked(lua, &node)?;

    file.replace(node.id, text)
        .map_err(|e| mlua::Error::runtime(e.to_string()))
}

pub(super) fn edit_remove(
    lua: &mlua::Lua,
    (_ctx, node): (mlua::Value, NodeRef),
) -> mlua::Result<()> {
    let file = checked(lua, &node)?;

    file.remove(node.id)
        .map_err(|e| mlua::Error::runtime(e.to_string()))
}
