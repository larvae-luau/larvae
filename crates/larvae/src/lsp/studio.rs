/*!
The mirror of a Roblox Studio place, and the Luau it declares.

A Studio plugin cannot listen on a port, so the plugin is the client: it POSTs
the live DataModel tree to larvae-lsp and reads the answer. This module holds
the half of that link with no HTTP in it. It decodes one message, folds it
into a tree, and says what the answer must carry.

`docs/PROTOCOL.md` of the plugin repository is the contract. Every rule below
names the section it comes from, because a mirror that drifts from the plugin
shows the user types for instances that no longer exist.

The mirror keeps a name, a class and a parent per node. It keeps nothing else,
because the plugin sends nothing else: no property, and no script source.

[`definitions`] turns the tree into `.d.luau` text. That text is the payoff.
It gives the type checker `game.Workspace.<Name>` with the class of the real
instance behind it.
*/

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

// --- the wire --------------------------------------------------------------

/// One node of the tree, in the short form the plugin sends
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct NodeWire {
    /// The node id. It is unique inside the session, and never reused.
    pub i: u32,
    /// The parent node id. The root reports 0.
    pub p: u32,
    /// The class index, one based, into the session class table
    pub c: u32,
    /// The `Name` of the instance
    pub n: String,
}

/// The plugin that opened the session
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Plugin {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

/// The place the session mirrors. `id` is 0 for a place that was never published.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Place {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub universe: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub studio: String,
}

/// `hello`: the plugin opens the session
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Hello {
    #[serde(default)]
    pub v: u32,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub plugin: Option<Plugin>,
    #[serde(default)]
    pub place: Option<Place>,
    /// The services the plugin watches. An empty list means every service.
    #[serde(default)]
    pub roots: Vec<String>,
}

/// `full`: one chunk of a whole snapshot
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Full {
    #[serde(default)]
    pub v: u32,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub seq: u64,
    /// The chunk number, from 1. Chunk 1 replaces the tree.
    #[serde(default)]
    pub chunk: u32,
    /*
    `final` on the wire, and a reserved word in Rust. The snapshot is
    complete when a chunk carries it.
    */
    #[serde(default, rename = "final")]
    pub last: bool,
    /// The root node id, in chunk 1 only. It is 1 for the DataModel.
    #[serde(default)]
    pub root: Option<u32>,
    #[serde(default)]
    pub place: Option<Place>,
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub nodes: Vec<NodeWire>,
}

/// One entry of `moved`: the node keeps its id and its name
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Moved {
    pub i: u32,
    pub p: u32,
}

/// One entry of `renamed`: the node keeps its id and its parent
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Renamed {
    pub i: u32,
    pub n: String,
}

/// `delta`: what changed since the last message. An absent field is an empty list.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Delta {
    #[serde(default)]
    pub v: u32,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub added: Vec<NodeWire>,
    #[serde(default)]
    pub moved: Vec<Moved>,
    #[serde(default)]
    pub renamed: Vec<Renamed>,
    /// The top of each removed branch. The server deletes the descendants.
    #[serde(default)]
    pub removed: Vec<u32>,
}

/// `bye`: Studio closed the place, or the user cut the link
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Bye {
    #[serde(default)]
    pub v: u32,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub seq: u64,
}

/*
One decoded message.

The `kind` field tags the four shapes, so serde reads the envelope and the
payload in one pass. The transport owns the version check: `v` rides along
here for a reader, and this module never branches on it.
*/
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Message {
    Hello(Hello),
    Full(Full),
    Delta(Delta),
    Bye(Bye),
}

impl Message {
    /// The session id of the envelope
    pub fn session(&self) -> &str {
        match self {
            Message::Hello(m) => &m.session,
            Message::Full(m) => &m.session,
            Message::Delta(m) => &m.session,
            Message::Bye(m) => &m.session,
        }
    }

    /// The sequence number of the envelope
    pub fn seq(&self) -> u64 {
        match self {
            Message::Hello(m) => m.seq,
            Message::Full(m) => m.seq,
            Message::Delta(m) => m.seq,
            Message::Bye(m) => m.seq,
        }
    }

    /// The `kind` string, for a log line
    pub fn kind(&self) -> &'static str {
        match self {
            Message::Hello(_) => "hello",
            Message::Full(_) => "full",
            Message::Delta(_) => "delta",
            Message::Bye(_) => "bye",
        }
    }
}

/*
What the answer must carry.

The protocol answers 200 with a JSON object, and `resync: true` makes the
plugin send the whole tree again. The transport turns [`Answer::Resync`] into
that field. This module writes no JSON.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Ok,
    Resync,
}

// --- the tree --------------------------------------------------------------

/// One instance of the mirrored place
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// The parent node id. The root reports 0.
    pub parent: u32,
    /// The class name, resolved through the session class table
    pub class: String,
    pub name: String,
}

/*
One Studio session, and the tree it mirrors.

The child lists live beside the nodes and not inside them. A removal names one
id and deletes a whole branch, so the walk down needs the children of a node
that it is deleting. One map of parent to children answers that in a single
lookup, and it keeps [`Node`] to the three fields the wire sends.
*/
#[derive(Debug, Clone, Default)]
pub struct Session {
    id: String,
    /*
    The last seq the session saw, 0 before the first message.

    The count rises by one per message, so a gap means a lost message. The
    session records the seq it saw even when it drops the message, because the
    plugin does not rewind its counter for the snapshot it sends next.
    */
    seq: u64,
    /*
    False until a message names this session.

    An unknown session in `full` or `delta` is a server restart. The first
    such message asks for a resync and marks the session known, so the tree
    the plugin sends next lands instead of asking again.
    */
    known: bool,
    /// The class table. A node's `c` is a one based index into it.
    classes: Vec<String>,
    nodes: HashMap<u32, Node>,
    /// The children of each node, in the order the plugin sent them
    kids: HashMap<u32, Vec<u32>>,
    root: u32,
    /// The chunk number of the snapshot that is arriving, 0 between snapshots
    chunk: u32,
    /// True once a snapshot ends with a `final` chunk
    complete: bool,
    /*
    How many times the tree changed.

    [`definitions`] names the types it writes after this number. The analyzer
    loads a definitions file into one global scope, and a second declaration
    of one type name fails the whole file. So each text carries fresh names.
    The number rises across a reset for the same reason.
    */
    revision: u64,
}

impl Session {
    pub fn new(id: String) -> Self {
        Self {
            id,
            ..Default::default()
        }
    }

    /*
    Apply one decoded message, and say what to answer.

    A message applies whole or not at all. The class check runs over the
    message before the first node lands, so a message that names a class the
    table cannot resolve leaves the tree as it was.
    */
    pub fn apply(&mut self, message: &Message) -> Answer {
        /*
        A message for another session belongs to another mirror. The
        transport routes by id, so this is a guard and not a path.
        */
        if message.session() != self.id {
            return Answer::Resync;
        }

        match message {
            Message::Hello(hello) => self.hello(hello),
            Message::Full(full) => self.full(full),
            Message::Delta(delta) => self.delta(delta),
            Message::Bye(bye) => self.bye(bye),
        }
    }

    /// The node ids of the children of a node, in the order the plugin sent them
    pub fn children(&self, id: u32) -> Vec<u32> {
        self.kids.get(&id).cloned().unwrap_or_default()
    }

    pub fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The last seq the session saw, 0 before the first message
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// The root node id, 1 for the DataModel. It is 0 before the first snapshot.
    pub fn root(&self) -> u32 {
        self.root
    }

    /// True once a snapshot ended with a `final` chunk
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// How many times the tree changed. The declarations name their types after it.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The class table, for a reader that reports the session
    pub fn classes(&self) -> &[String] {
        &self.classes
    }

    // --- one kind of message ----------------------------------------------

    /*
    `hello` opens the session.

    A `hello` for a session id the server already knows means Studio reloaded
    the plugin. The old tree describes the place that the reload left, so the
    session drops it and starts empty. The class table starts empty too.
    */
    fn hello(&mut self, hello: &Hello) -> Answer {
        self.reset();

        self.known = true;
        self.seq = hello.seq;

        Answer::Ok
    }

    /*
    `full` carries the whole tree, in chunks.

    Chunk 1 replaces the tree and drops the old class table, so a snapshot
    never mixes with the one before it. A parent always arrives before its
    children, so the child lists build in one pass.
    */
    fn full(&mut self, full: &Full) -> Answer {
        if !self.known {
            return self.restart(full.seq);
        }

        if !self.step(full.seq) {
            self.chunk = 0;

            return Answer::Resync;
        }

        // Chunk 1 opens a snapshot. Any other chunk continues the open one.
        let expected = if self.chunk == 0 { 1 } else { self.chunk + 1 };

        if full.chunk != expected {
            self.chunk = 0;

            return Answer::Resync;
        }

        // The table empties at chunk 1, so the count a `c` may reach changes.
        let base = if full.chunk == 1 {
            0
        } else {
            self.classes.len()
        };

        if !resolves(&full.nodes, base + full.classes.len()) {
            self.chunk = 0;

            return Answer::Resync;
        }

        if full.chunk == 1 {
            self.classes.clear();
            self.nodes.clear();
            self.kids.clear();
            self.complete = false;
            self.root = full.root.unwrap_or(1);
        }

        self.classes.extend(full.classes.iter().cloned());

        for wire in &full.nodes {
            self.put(wire);
        }

        self.chunk = if full.last { 0 } else { full.chunk };
        self.complete = full.last;
        self.revision += 1;

        Answer::Ok
    }

    /*
    `delta` carries what changed, in four lists.

    The order is `added`, `moved`, `renamed`, `removed`, and it is the reason
    the lists are separate. A node that leaves a branch which the same message
    deletes appears in `moved`, and `removed` runs last, so the node lives.
    */
    fn delta(&mut self, delta: &Delta) -> Answer {
        if !self.known {
            return self.restart(delta.seq);
        }

        if !self.step(delta.seq) {
            return Answer::Resync;
        }

        if !resolves(&delta.added, self.classes.len() + delta.classes.len()) {
            return Answer::Resync;
        }

        self.classes.extend(delta.classes.iter().cloned());

        for wire in &delta.added {
            self.put(wire);
        }

        for moved in &delta.moved {
            self.reparent(moved.i, moved.p);
        }

        for renamed in &delta.renamed {
            if let Some(node) = self.nodes.get_mut(&renamed.i) {
                node.name.clone_from(&renamed.n);
            }
        }

        for id in &delta.removed {
            self.cut(*id);
        }

        self.revision += 1;

        Answer::Ok
    }

    /*
    `bye` ends the session.

    The tree describes a place that is closed, so it goes. The session reads
    as unknown after this, because a `bye` can be lost and a `delta` can
    arrive behind it. An unknown session asks for a resync, which is the
    answer a closed place deserves.
    */
    fn bye(&mut self, bye: &Bye) -> Answer {
        self.reset();

        self.seq = bye.seq;

        Answer::Ok
    }

    // --- the parts the kinds share ----------------------------------------

    /*
    Empty the session, and keep the id and the revision.

    The revision rises across the reset because the analyzer keeps the types
    of the former tree in its global scope. A revision that started again
    would declare a type name that scope already holds.
    */
    fn reset(&mut self) {
        let id = std::mem::take(&mut self.id);
        let revision = self.revision + 1;

        *self = Self::new(id);
        self.revision = revision;
    }

    /*
    Take the seq of a message the session drops, and ask for a resync.

    The session is known after this, so the snapshot the plugin sends next
    applies. Without it the answer repeats and the link carries no tree.
    */
    fn restart(&mut self, seq: u64) -> Answer {
        self.known = true;
        self.seq = seq;

        Answer::Resync
    }

    /*
    Move the seq forward, and report whether the count held.

    The session adopts the seq it saw even on a gap. The plugin counts its own
    messages and does not rewind, so a session that kept the old number would
    read the resent snapshot as another gap.
    */
    fn step(&mut self, seq: u64) -> bool {
        let expected = self.seq + 1;

        self.seq = seq;

        seq == expected
    }

    /// Put one node in the tree, and file it under its parent
    fn put(&mut self, wire: &NodeWire) {
        // An id never repeats inside a session, so this only guards the map.
        self.cut(wire.i);

        let class = self
            .classes
            .get(wire.c as usize - 1)
            .cloned()
            .unwrap_or_default();

        self.nodes.insert(
            wire.i,
            Node {
                parent: wire.p,
                class,
                name: wire.n.clone(),
            },
        );

        self.kids.entry(wire.p).or_default().push(wire.i);
    }

    /// Give a node a new parent. It keeps its id, its name and its children.
    fn reparent(&mut self, id: u32, parent: u32) {
        let Some(node) = self.nodes.get_mut(&id) else {
            return;
        };

        let old = node.parent;

        node.parent = parent;

        if let Some(list) = self.kids.get_mut(&old) {
            list.retain(|&kid| kid != id);
        }

        self.kids.entry(parent).or_default().push(id);
    }

    /*
    Delete a node and every descendant of it.

    A removal names the top of the branch only. A folder that holds ten
    thousand parts travels as one id, so the walk down belongs here.

    The walk takes a node out of the map before it reads the children of that
    node, so a parent loop cannot spin.
    */
    fn cut(&mut self, id: u32) {
        let Some(node) = self.nodes.remove(&id) else {
            return;
        };

        if let Some(list) = self.kids.get_mut(&node.parent) {
            list.retain(|&kid| kid != id);
        }

        let mut stack = vec![id];

        while let Some(top) = stack.pop() {
            let Some(kids) = self.kids.remove(&top) else {
                continue;
            };

            for kid in kids {
                if self.nodes.remove(&kid).is_some() {
                    stack.push(kid);
                }
            }
        }
    }
}

/*
Whether every `c` of a list lands inside the class table.

A message that names a new class carries that class, so a `c` past the end of
the table means the message is malformed or a message was lost. The caller
answers with a resync and leaves the tree alone, because half a message
describes a place that never existed.
*/
fn resolves(nodes: &[NodeWire], classes: usize) -> bool {
    nodes
        .iter()
        .all(|node| node.c >= 1 && node.c as usize <= classes)
}

// --- the declarations ------------------------------------------------------

/*
The Luau declaration text for the mirrored tree.

The analyzer loads a `.d.luau` into its global scope, and that scope is the
one seam a language server has for the shape of a place. So each node that has
children becomes a declared type, and `game` takes the type of the root:

```luau
declare extern type _larvae_3f2a_2_2 extends Workspace with
    Baseplate: Part
end

declare extern type _larvae_3f2a_2_1 extends DataModel with
    Workspace: _larvae_3f2a_2_2
end

declare game: _larvae_3f2a_2_1
declare workspace: _larvae_3f2a_2_2
```

A declared type extends the class of the instance, so a child keeps every
member the platform gives it and gains the children of the place.

The text describes the mirror as it stands. Write it after a snapshot
completes, or the reader sees a tree that is still arriving.

Three names stay out, because a wrong type is worse than no type:

- a name that is not a Luau identifier, which cannot be a field at all;
- a second child with a name a sibling already used, since one field cannot
  hold two types;
- a name that every instance already carries, `Name` and `Parent` among them,
  because a field of that name hides the real member.
*/
pub fn definitions(session: &Session) -> String {
    let root = session.root();

    if session.node(root).is_none() {
        return String::new();
    }

    /*
    Read the tree parent first, then write it child first.

    A declared type names the type of each child, so the child must be
    declared already. The reverse of a walk down the tree gives that order in
    one pass, with no sort. The `seen` set holds because a malformed move can
    still point a branch at itself.
    */
    let mut order = Vec::with_capacity(session.len());
    let mut seen = HashSet::new();
    let mut stack = vec![root];

    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }

        order.push(id);
        stack.extend(session.children(id));
    }

    let mut named: HashMap<u32, String> = HashMap::new();
    let mut body = String::new();

    for id in order.iter().rev() {
        let fields = fields_of(session, *id, &named);

        if fields.is_empty() {
            continue;
        }

        let Some(node) = session.node(*id) else {
            continue;
        };

        let name = format!(
            "_larvae_{}_{}_{}",
            tag(session.id()),
            session.revision(),
            id
        );

        let _ = writeln!(
            body,
            "declare extern type {name} extends {} with",
            class_of(&node.class)
        );

        for (field, ty) in fields {
            let _ = writeln!(body, "\t{field}: {ty}");
        }

        body.push_str("end\n\n");
        named.insert(*id, name);
    }

    // A root with no field of its own declares nothing a reader can use.
    let Some(game) = named.get(&root) else {
        return String::new();
    };

    let mut text = String::with_capacity(body.len() + 256);

    let _ = writeln!(
        text,
        "-- The Roblox Studio tree larvae mirrors. Do not edit."
    );
    let _ = writeln!(
        text,
        "-- session {}, revision {}, {} nodes",
        tag(session.id()),
        session.revision(),
        session.len()
    );

    text.push('\n');
    text.push_str(&body);

    let _ = writeln!(text, "declare game: {game}");

    /*
    `workspace` is the same instance under a second global name. A place with
    no Workspace leaves that global as the platform declares it.
    */
    if let Some(ty) = child_type(session, root, "Workspace", &named) {
        let _ = writeln!(text, "declare workspace: {ty}");
    }

    text
}

/*
The fields one declared type carries, in the order the children arrived.

A child with a declared type of its own takes that name. A child with no
children takes its class, so a leaf still types as the instance it is.
*/
fn fields_of(session: &Session, id: u32, named: &HashMap<u32, String>) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let mut used = HashSet::new();

    for kid in session.children(id) {
        let Some(child) = session.node(kid) else {
            continue;
        };

        if !field_name(&child.name) {
            continue;
        }

        if !used.insert(child.name.clone()) {
            continue;
        }

        let ty = named
            .get(&kid)
            .cloned()
            .unwrap_or_else(|| class_of(&child.class).to_owned());

        fields.push((child.name.clone(), ty));
    }

    fields
}

/// The type of one named child of a node, for a global that points at it
fn child_type(
    session: &Session,
    parent: u32,
    name: &str,
    named: &HashMap<u32, String>,
) -> Option<String> {
    for kid in session.children(parent) {
        let Some(child) = session.node(kid) else {
            continue;
        };

        if child.name != name {
            continue;
        }

        return Some(
            named
                .get(&kid)
                .cloned()
                .unwrap_or_else(|| class_of(&child.class).to_owned()),
        );
    }

    None
}

/*
The type name for a class name.

The platform declares one type per class, so the class name is the type name.
A class the mirror cannot spell as a type falls back to `Instance`, which
every instance is. The alternative writes a name that no definitions file
declares, and one such name fails the whole file.

The check reads the shape and holds no list. A class list here would drift
from the types of the analyzer on every Studio release.
*/
fn class_of(class: &str) -> &str {
    let mut chars = class.chars();

    let Some(first) = chars.next() else {
        return "Instance";
    };

    if !first.is_ascii_uppercase() {
        return "Instance";
    }

    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return "Instance";
    }

    class
}

/// Whether a `Name` can be a field of a declared type
fn field_name(name: &str) -> bool {
    let mut chars = name.chars();

    let Some(first) = chars.next() else {
        return false;
    };

    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }

    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }

    if RESERVED.contains(&name) {
        return false;
    }

    MEMBERS.binary_search(&name).is_err()
}

/*
A short tag for the session id.

The tag names the declared types, so two Studio sessions cannot collide in the
global scope of one analyzer. It also rides in a comment, where a raw id would
end the line at the first newline it holds. The hash is FNV-1a: it needs no
dependency, and nothing here defends against a chosen collision.
*/
fn tag(id: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;

    for byte in id.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }

    format!("{hash:08x}")
}

/// The words Luau reserves. A field cannot carry one.
const RESERVED: [&str; 21] = [
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/*
Every member `Instance` and `Object` give. A field of one of these names hides
the real member, so a child that carries one stays out of the declarations.

The list is sorted for a binary search, and it comes from
`crates/larvae-lsp/types/globalTypes.d.luau`.
*/
const MEMBERS: [&str; 60] = [
    "AddTag",
    "AncestryChanged",
    "Archivable",
    "AttributeChanged",
    "Capabilities",
    "Changed",
    "ChildAdded",
    "ChildRemoved",
    "ClassName",
    "ClearAllChildren",
    "Clone",
    "DescendantAdded",
    "DescendantRemoving",
    "Destroy",
    "Destroying",
    "FindFirstAncestor",
    "FindFirstAncestorOfClass",
    "FindFirstAncestorWhichIsA",
    "FindFirstChild",
    "FindFirstChildOfClass",
    "FindFirstChildWhichIsA",
    "FindFirstDescendant",
    "GetActor",
    "GetAttribute",
    "GetAttributeChangedSignal",
    "GetAttributes",
    "GetChildren",
    "GetDebugId",
    "GetDescendants",
    "GetFullName",
    "GetPropertyChangedSignal",
    "GetStyled",
    "GetStyledPropertyChangedSignal",
    "GetTags",
    "HasTag",
    "IsA",
    "IsAncestorOf",
    "IsDescendantOf",
    "IsPropertyModified",
    "Name",
    "Parent",
    "QueryDescendants",
    "Remove",
    "RemoveTag",
    "ResetPropertyToDefault",
    "RobloxLocked",
    "Sandboxed",
    "SetAttribute",
    "SourceAssetId",
    "StyledPropertiesChanged",
    "UniqueId",
    "WaitForChild",
    "children",
    "clone",
    "destroy",
    "findFirstChild",
    "getChildren",
    "isA",
    "isDescendantOf",
    "remove",
];

/*
A tree and its declaration text, for a caller that wants to try the text
against a real type checker.

The generator is verified against larvae's own definitions parser in the
tests above. That parser accepts the syntax and says nothing about whether
Luau's frontend accepts the meaning, and the two are different questions:
a shadowed inherited property is legal syntax either way.
*/
#[doc(hidden)]
pub fn sample_place() -> Session {
    let mut session = Session::new("sample".to_string());

    let wire = |i, p, c, n: &str| NodeWire {
        i,
        p,
        c,
        n: n.to_string(),
    };

    /*
    The hello comes first, as it does on the wire. A snapshot for a session
    larvae never greeted is a restart, and the model answers it with a
    resync rather than a tree.
    */
    session.apply(&Message::Hello(Hello {
        v: 1,
        session: "sample".to_string(),
        seq: 1,
        plugin: None,
        place: None,
        roots: Vec::new(),
    }));

    let full = Full {
        v: 1,
        session: "sample".to_string(),
        seq: 2,
        chunk: 1,
        last: true,
        root: Some(1),
        place: None,
        classes: vec![
            "DataModel".into(),
            "Workspace".into(),
            "Part".into(),
            "Folder".into(),
            "ModuleScript".into(),
            "ReplicatedStorage".into(),
        ],
        nodes: vec![
            wire(1, 0, 1, "Place"),
            wire(2, 1, 2, "Workspace"),
            wire(3, 2, 3, "Baseplate"),
            wire(4, 1, 6, "ReplicatedStorage"),
            wire(5, 4, 4, "Modules"),
            wire(6, 5, 5, "Util"),
        ],
    };

    session.apply(&Message::Full(full));

    session
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::syntax::{lexer, parser};

    use super::*;

    const ID: &str = "8f1c-0000-4444";

    fn decode(value: Value) -> Message {
        serde_json::from_value(value).expect("the message decodes")
    }

    fn hello(seq: u64) -> Message {
        decode(json!({
            "v": 1, "kind": "hello", "session": ID, "seq": seq,
            "plugin": { "name": "larvae-studio", "version": "0.1.0" },
            "roots": ["Workspace"],
        }))
    }

    /// A session that read a `hello` and waits for a snapshot
    fn opened() -> Session {
        let mut session = Session::new(ID.to_owned());

        assert_eq!(session.apply(&hello(1)), Answer::Ok);

        session
    }

    /// The three node place the tests build on: the DataModel, Workspace, a Part
    fn baseplate(seq: u64) -> Message {
        decode(json!({
            "v": 1, "kind": "full", "session": ID, "seq": seq,
            "chunk": 1, "final": true, "root": 1,
            "classes": ["DataModel", "Workspace", "Part"],
            "nodes": [
                { "i": 1, "p": 0, "c": 1, "n": "Baseplate" },
                { "i": 2, "p": 1, "c": 2, "n": "Workspace" },
                { "i": 3, "p": 2, "c": 3, "n": "Baseplate" },
            ],
        }))
    }

    /// A session that holds the three node place
    fn mirrored() -> Session {
        let mut session = opened();

        assert_eq!(session.apply(&baseplate(2)), Answer::Ok);

        session
    }

    fn names(session: &Session, id: u32) -> Vec<String> {
        session
            .children(id)
            .into_iter()
            .filter_map(|kid| session.node(kid).map(|node| node.name.clone()))
            .collect()
    }

    // --- the envelope -----------------------------------------------------

    /// The four shapes of the protocol document decode as they are written
    #[test]
    fn the_protocol_examples_decode() {
        let hello = decode(json!({
            "v": 1, "kind": "hello", "session": "8f1c", "seq": 1,
            "plugin": { "name": "larvae-studio", "version": "0.1.0" },
            "place": { "id": 1818, "universe": 0, "name": "Baseplate", "studio": "0.680.0" },
            "roots": ["Workspace", "ReplicatedStorage"],
        }));

        assert_eq!(hello.kind(), "hello");
        assert_eq!(hello.session(), "8f1c");
        assert_eq!(hello.seq(), 1);

        let full = decode(json!({
            "v": 1, "kind": "full", "session": "8f1c", "seq": 2,
            "chunk": 1, "final": false, "root": 1,
            "place": { "id": 1818, "universe": 0, "name": "Baseplate", "studio": "0.680.0" },
            "classes": ["DataModel", "Workspace", "Part"],
            "nodes": [
                { "i": 1, "p": 0, "c": 1, "n": "Baseplate" },
                { "i": 2, "p": 1, "c": 2, "n": "Workspace" },
                { "i": 3, "p": 2, "c": 3, "n": "Baseplate" },
            ],
        }));

        let Message::Full(full) = &full else {
            panic!("the kind tag names a full");
        };

        assert_eq!(full.chunk, 1);
        assert!(!full.last, "final is false here");
        assert_eq!(full.root, Some(1));
        assert_eq!(full.nodes.len(), 3);

        let delta = decode(json!({
            "v": 1, "kind": "delta", "session": "8f1c", "seq": 7,
            "classes": ["Folder"],
            "added": [{ "i": 41, "p": 2, "c": 4, "n": "Enemies" }],
            "moved": [{ "i": 33, "p": 41 }],
            "renamed": [{ "i": 12, "n": "Hitbox" }],
            "removed": [28],
        }));

        let Message::Delta(delta) = &delta else {
            panic!("the kind tag names a delta");
        };

        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.moved[0].p, 41);
        assert_eq!(delta.renamed[0].n, "Hitbox");
        assert_eq!(delta.removed, vec![28]);

        let bye = decode(json!({ "v": 1, "kind": "bye", "session": "8f1c", "seq": 12 }));

        assert_eq!(bye.kind(), "bye");
    }

    /// An absent list is an empty list
    #[test]
    fn a_delta_without_a_list_carries_nothing() {
        let message = decode(json!({ "kind": "delta", "session": ID, "seq": 3 }));

        let Message::Delta(delta) = &message else {
            panic!("the kind tag names a delta");
        };

        assert!(delta.added.is_empty());
        assert!(delta.moved.is_empty());
        assert!(delta.renamed.is_empty());
        assert!(delta.removed.is_empty());
    }

    // --- rule 1, the count ------------------------------------------------

    #[test]
    fn a_gap_in_the_count_asks_for_a_resync() {
        let mut session = mirrored();

        // The count stands at 2, so 4 means the message with 3 was lost.
        let delta = decode(json!({
            "v": 1, "kind": "delta", "session": ID, "seq": 4,
            "removed": [3],
        }));

        assert_eq!(session.apply(&delta), Answer::Resync);
        assert_eq!(session.len(), 3, "a dropped message changes nothing");
        assert!(session.node(3).is_some());
    }

    /*
    The snapshot that follows a gap lands.

    The plugin counts its own messages and does not rewind, so the session
    takes the seq of the message it dropped.
    */
    #[test]
    fn the_snapshot_after_a_gap_lands() {
        let mut session = mirrored();

        let delta = decode(json!({ "kind": "delta", "session": ID, "seq": 4 }));

        assert_eq!(session.apply(&delta), Answer::Resync);
        assert_eq!(session.seq(), 4);
        assert_eq!(session.apply(&baseplate(5)), Answer::Ok);
    }

    #[test]
    fn a_count_that_repeats_asks_for_a_resync() {
        let mut session = mirrored();

        assert_eq!(session.apply(&baseplate(2)), Answer::Resync);
    }

    // --- rule 2, the class table ------------------------------------------

    /// A `c` counts from one, and the table keeps what an earlier chunk named
    #[test]
    fn a_class_index_is_one_based_and_the_table_appends() {
        let mut session = opened();

        let first = decode(json!({
            "kind": "full", "session": ID, "seq": 2, "chunk": 1, "final": false, "root": 1,
            "classes": ["DataModel", "Workspace"],
            "nodes": [
                { "i": 1, "p": 0, "c": 1, "n": "Baseplate" },
                { "i": 2, "p": 1, "c": 2, "n": "Workspace" },
            ],
        }));

        assert_eq!(session.apply(&first), Answer::Ok);

        // The second chunk names one new class, and reads one the first named.
        let second = decode(json!({
            "kind": "full", "session": ID, "seq": 3, "chunk": 2, "final": true,
            "classes": ["Part"],
            "nodes": [
                { "i": 3, "p": 2, "c": 3, "n": "Rock" },
                { "i": 4, "p": 2, "c": 2, "n": "Second" },
            ],
        }));

        assert_eq!(session.apply(&second), Answer::Ok);
        assert_eq!(session.classes(), ["DataModel", "Workspace", "Part"]);
        assert_eq!(session.node(1).unwrap().class, "DataModel");
        assert_eq!(session.node(3).unwrap().class, "Part");
        assert_eq!(session.node(4).unwrap().class, "Workspace");
    }

    /// Chunk 1 of a snapshot drops the table the snapshot before it built
    #[test]
    fn a_snapshot_empties_the_class_table() {
        let mut session = mirrored();

        assert_eq!(session.classes().len(), 3);

        let second = decode(json!({
            "kind": "full", "session": ID, "seq": 3, "chunk": 1, "final": true, "root": 1,
            "classes": ["DataModel", "Workspace"],
            "nodes": [
                { "i": 1, "p": 0, "c": 1, "n": "Baseplate" },
                { "i": 2, "p": 1, "c": 2, "n": "Workspace" },
            ],
        }));

        assert_eq!(session.apply(&second), Answer::Ok);
        assert_eq!(session.classes(), ["DataModel", "Workspace"]);
    }

    /*
    A `c` past the end of the table is a malformed message.

    The session answers with a resync and applies none of the message. A
    message that names a new class carries it, so an index that misses means
    the message lost a part of itself.
    */
    #[test]
    fn a_class_index_that_misses_asks_for_a_resync() {
        let mut session = mirrored();

        let delta = decode(json!({
            "kind": "delta", "session": ID, "seq": 3,
            "added": [
                { "i": 4, "p": 2, "c": 3, "n": "Good" },
                { "i": 5, "p": 2, "c": 9, "n": "Bad" },
            ],
        }));

        assert_eq!(session.apply(&delta), Answer::Resync);
        assert_eq!(session.len(), 3, "the whole message stays out");
        assert!(session.node(4).is_none(), "the first node stays out too");
    }

    /// A `c` of zero misses, because the index counts from one
    #[test]
    fn a_class_index_of_zero_asks_for_a_resync() {
        let mut session = mirrored();

        let delta = decode(json!({
            "kind": "delta", "session": ID, "seq": 3,
            "added": [{ "i": 4, "p": 2, "c": 0, "n": "Bad" }],
        }));

        assert_eq!(session.apply(&delta), Answer::Resync);
        assert_eq!(session.len(), 3);
    }

    // --- rule 3, the chunks -----------------------------------------------

    #[test]
    fn chunk_one_replaces_the_tree() {
        let mut session = mirrored();

        let second = decode(json!({
            "kind": "full", "session": ID, "seq": 3, "chunk": 1, "final": true, "root": 1,
            "classes": ["DataModel", "Lighting"],
            "nodes": [
                { "i": 1, "p": 0, "c": 1, "n": "Other" },
                { "i": 7, "p": 1, "c": 2, "n": "Lighting" },
            ],
        }));

        assert_eq!(session.apply(&second), Answer::Ok);
        assert_eq!(session.len(), 2);
        assert!(session.node(3).is_none(), "the old tree is gone");
        assert_eq!(names(&session, 1), ["Lighting"]);
    }

    #[test]
    fn a_snapshot_completes_on_the_final_chunk() {
        let mut session = opened();

        let first = decode(json!({
            "kind": "full", "session": ID, "seq": 2, "chunk": 1, "final": false, "root": 1,
            "classes": ["DataModel"],
            "nodes": [{ "i": 1, "p": 0, "c": 1, "n": "Baseplate" }],
        }));

        assert_eq!(session.apply(&first), Answer::Ok);
        assert!(!session.complete(), "one chunk of two is not a tree");

        let second = decode(json!({
            "kind": "full", "session": ID, "seq": 3, "chunk": 2, "final": true,
            "classes": ["Workspace"],
            "nodes": [{ "i": 2, "p": 1, "c": 2, "n": "Workspace" }],
        }));

        assert_eq!(session.apply(&second), Answer::Ok);
        assert!(session.complete());
        assert_eq!(session.root(), 1);
        assert_eq!(session.len(), 2);
    }

    /// A chunk that skips its place in the snapshot asks for a resync
    #[test]
    fn a_chunk_out_of_order_asks_for_a_resync() {
        let mut session = opened();

        let third = decode(json!({
            "kind": "full", "session": ID, "seq": 2, "chunk": 3, "final": true,
            "classes": ["DataModel"],
            "nodes": [{ "i": 1, "p": 0, "c": 1, "n": "Baseplate" }],
        }));

        assert_eq!(session.apply(&third), Answer::Resync);
        assert!(session.is_empty());
    }

    /// A parent arrives before its children, so one pass builds the child lists
    #[test]
    fn a_child_files_under_the_parent_that_came_first() {
        let session = mirrored();

        assert_eq!(names(&session, 1), ["Workspace"]);
        assert_eq!(names(&session, 2), ["Baseplate"]);
        assert_eq!(session.node(3).unwrap().parent, 2);
    }

    // --- rule 4, the order of a delta -------------------------------------

    /*
    A node that leaves a branch the same message deletes lives.

    `moved` runs before `removed`, so the node is out of the branch when the
    removal walks it. This is the case the protocol document names.
    */
    #[test]
    fn a_node_that_moves_out_of_a_removed_branch_lives() {
        let mut session = opened();

        let tree = decode(json!({
            "kind": "full", "session": ID, "seq": 2, "chunk": 1, "final": true, "root": 1,
            "classes": ["DataModel", "Workspace", "Folder", "Part"],
            "nodes": [
                { "i": 1, "p": 0, "c": 1, "n": "Baseplate" },
                { "i": 2, "p": 1, "c": 2, "n": "Workspace" },
                { "i": 3, "p": 2, "c": 3, "n": "Old" },
                { "i": 4, "p": 3, "c": 4, "n": "Keep" },
                { "i": 5, "p": 3, "c": 4, "n": "Drop" },
            ],
        }));

        assert_eq!(session.apply(&tree), Answer::Ok);

        let delta = decode(json!({
            "kind": "delta", "session": ID, "seq": 3,
            "moved": [{ "i": 4, "p": 2 }],
            "removed": [3],
        }));

        assert_eq!(session.apply(&delta), Answer::Ok);
        assert!(session.node(3).is_none(), "the branch is gone");
        assert!(session.node(5).is_none(), "the node that stayed is gone");
        assert_eq!(session.node(4).unwrap().parent, 2, "the mover lives");
        assert_eq!(names(&session, 2), ["Keep"]);
    }

    /// `added` runs first, so a node added into a branch and moved out lives
    #[test]
    fn the_four_lists_run_in_their_order() {
        let mut session = mirrored();

        let delta = decode(json!({
            "kind": "delta", "session": ID, "seq": 3,
            "classes": ["Folder"],
            "added": [{ "i": 4, "p": 3, "c": 4, "n": "Fresh" }],
            "moved": [{ "i": 4, "p": 2 }],
            "renamed": [{ "i": 4, "n": "Renamed" }],
            "removed": [3],
        }));

        assert_eq!(session.apply(&delta), Answer::Ok);
        assert!(session.node(3).is_none());

        let node = session.node(4).expect("the added node lives");

        assert_eq!(node.parent, 2);
        assert_eq!(node.name, "Renamed");
        assert_eq!(node.class, "Folder");
    }

    #[test]
    fn a_rename_keeps_the_id_and_the_parent() {
        let mut session = mirrored();

        let delta = decode(json!({
            "kind": "delta", "session": ID, "seq": 3,
            "renamed": [{ "i": 3, "n": "Hitbox" }],
        }));

        assert_eq!(session.apply(&delta), Answer::Ok);

        let node = session.node(3).unwrap();

        assert_eq!(node.name, "Hitbox");
        assert_eq!(node.parent, 2);
        assert_eq!(node.class, "Part");
    }

    /// A move keeps the id, so what the server derived for the branch stays
    #[test]
    fn a_move_keeps_the_branch_under_the_node() {
        let mut session = opened();

        let tree = decode(json!({
            "kind": "full", "session": ID, "seq": 2, "chunk": 1, "final": true, "root": 1,
            "classes": ["DataModel", "Workspace", "Folder", "Part"],
            "nodes": [
                { "i": 1, "p": 0, "c": 1, "n": "Baseplate" },
                { "i": 2, "p": 1, "c": 2, "n": "Workspace" },
                { "i": 3, "p": 2, "c": 3, "n": "Enemies" },
                { "i": 4, "p": 3, "c": 3, "n": "Wave" },
                { "i": 5, "p": 4, "c": 4, "n": "Body" },
            ],
        }));

        assert_eq!(session.apply(&tree), Answer::Ok);

        let delta = decode(json!({
            "kind": "delta", "session": ID, "seq": 3,
            "moved": [{ "i": 3, "p": 1 }],
        }));

        assert_eq!(session.apply(&delta), Answer::Ok);
        assert_eq!(names(&session, 1), ["Workspace", "Enemies"]);
        assert!(names(&session, 2).is_empty());
        assert_eq!(names(&session, 4), ["Body"]);
        assert_eq!(session.len(), 5);
    }

    // --- rule 5, a removal is a branch ------------------------------------

    /// One id removes a whole branch, however deep it runs
    #[test]
    fn a_removal_takes_every_descendant() {
        let mut session = opened();

        let mut nodes = vec![json!({ "i": 1, "p": 0, "c": 1, "n": "Baseplate" })];

        // A chain of 64 folders under the DataModel, and a part at the end.
        for id in 2..66_u32 {
            nodes.push(json!({ "i": id, "p": id - 1, "c": 2, "n": format!("Level{id}") }));
        }

        nodes.push(json!({ "i": 66, "p": 65, "c": 3, "n": "Deep" }));

        let tree = decode(json!({
            "kind": "full", "session": ID, "seq": 2, "chunk": 1, "final": true, "root": 1,
            "classes": ["DataModel", "Folder", "Part"],
            "nodes": nodes,
        }));

        assert_eq!(session.apply(&tree), Answer::Ok);
        assert_eq!(session.len(), 66);

        let delta = decode(json!({
            "kind": "delta", "session": ID, "seq": 3,
            "removed": [2],
        }));

        assert_eq!(session.apply(&delta), Answer::Ok);
        assert_eq!(session.len(), 1, "only the DataModel is left");
        assert!(session.node(66).is_none());
        assert!(names(&session, 1).is_empty(), "the child list drops it too");
    }

    /// A removal of an id the mirror never held changes nothing
    #[test]
    fn a_removal_of_an_unknown_id_changes_nothing() {
        let mut session = mirrored();

        let delta = decode(json!({
            "kind": "delta", "session": ID, "seq": 3,
            "removed": [404],
        }));

        assert_eq!(session.apply(&delta), Answer::Ok);
        assert_eq!(session.len(), 3);
    }

    // --- rule 6, a second hello -------------------------------------------

    /// Studio reloaded the plugin, so the session starts again and empty
    #[test]
    fn a_second_hello_resets_the_session() {
        let mut session = mirrored();

        assert_eq!(session.len(), 3);

        assert_eq!(session.apply(&hello(1)), Answer::Ok);
        assert!(session.is_empty());
        assert!(session.classes().is_empty());
        assert!(!session.complete());
        assert_eq!(session.seq(), 1);
        assert_eq!(
            session.apply(&baseplate(2)),
            Answer::Ok,
            "the count starts again"
        );
    }

    // --- rule 7, an unknown session ---------------------------------------

    /// The server restarted, so it holds no tree for the id the plugin sends
    #[test]
    fn a_snapshot_for_an_unknown_session_asks_for_a_resync() {
        let mut session = Session::new(ID.to_owned());

        assert_eq!(session.apply(&baseplate(9)), Answer::Resync);
        assert!(session.is_empty());

        // The session knows the id now, so the tree the plugin resends lands.
        assert_eq!(session.apply(&baseplate(10)), Answer::Ok);
        assert_eq!(session.len(), 3);
    }

    #[test]
    fn a_delta_for_an_unknown_session_asks_for_a_resync() {
        let mut session = Session::new(ID.to_owned());

        let delta = decode(json!({
            "kind": "delta", "session": ID, "seq": 5,
            "removed": [3],
        }));

        assert_eq!(session.apply(&delta), Answer::Resync);
        assert!(session.is_empty());
    }

    /// A message for another id belongs to another mirror
    #[test]
    fn a_message_for_another_session_asks_for_a_resync() {
        let mut session = mirrored();

        let stray = decode(json!({
            "kind": "delta", "session": "other", "seq": 3,
            "removed": [3],
        }));

        assert_eq!(session.apply(&stray), Answer::Resync);
        assert_eq!(session.len(), 3);
    }

    // --- bye --------------------------------------------------------------

    /*
    The place is closed, so the tree goes.

    A `bye` can be lost and a `delta` can arrive behind it. The session reads
    as unknown after the `bye`, so that late `delta` asks for a resync.
    */
    #[test]
    fn a_bye_drops_the_tree() {
        let mut session = mirrored();

        let bye = decode(json!({ "kind": "bye", "session": ID, "seq": 3 }));

        assert_eq!(session.apply(&bye), Answer::Ok);
        assert!(session.is_empty());

        let late = decode(json!({
            "kind": "delta", "session": ID, "seq": 4,
            "removed": [3],
        }));

        assert_eq!(session.apply(&late), Answer::Resync);
    }

    // --- the declarations -------------------------------------------------

    /// The tree the declaration tests read
    fn place() -> Session {
        let mut session = opened();

        let tree = decode(json!({
            "kind": "full", "session": ID, "seq": 2, "chunk": 1, "final": true, "root": 1,
            "classes": ["DataModel", "Workspace", "Part", "Folder", "ModuleScript"],
            "nodes": [
                { "i": 1, "p": 0, "c": 1, "n": "Baseplate" },
                { "i": 2, "p": 1, "c": 2, "n": "Workspace" },
                { "i": 3, "p": 2, "c": 3, "n": "Baseplate" },
                { "i": 4, "p": 2, "c": 4, "n": "Modules" },
                { "i": 5, "p": 4, "c": 5, "n": "Util" },
            ],
        }));

        assert_eq!(session.apply(&tree), Answer::Ok);

        session
    }

    /// The text must be a definitions file that a language server can load
    fn parses(text: &str) {
        let lexed = lexer::lex(text).expect("the declarations lex");

        parser::parse_with(
            text,
            &lexed.toks,
            parser::ParseOptions {
                definitions: true,
                ..Default::default()
            },
        )
        .expect("the declarations parse");
    }

    #[test]
    fn the_declarations_type_the_children_of_a_service() {
        let session = place();
        let text = definitions(&session);

        parses(&text);

        assert!(text.contains("declare game: _larvae_"), "{text}");
        assert!(text.contains("declare workspace: _larvae_"), "{text}");
        assert!(text.contains("extends DataModel with"), "{text}");
        assert!(text.contains("extends Workspace with"), "{text}");
        assert!(text.contains("extends Folder with"), "{text}");
        assert!(text.contains("\tBaseplate: Part\n"), "{text}");
        assert!(text.contains("\tUtil: ModuleScript\n"), "{text}");

        /*
        A declared type names the type of each child, so the child comes
        first. A definitions file that reads a name it never declared fails,
        and one failure loses the whole place.
        */
        let folder = text.find("extends Folder").expect("the folder is declared");
        let workspace = text
            .find("extends Workspace")
            .expect("the service is declared");
        let game = text
            .find("extends DataModel")
            .expect("the root is declared");

        assert!(folder < workspace, "{text}");
        assert!(workspace < game, "{text}");
    }

    /// A name that cannot be a field stays out, and the rest of the text holds
    #[test]
    fn a_name_that_is_not_an_identifier_stays_out() {
        let mut session = opened();

        let tree = decode(json!({
            "kind": "full", "session": ID, "seq": 2, "chunk": 1, "final": true, "root": 1,
            "classes": ["DataModel", "Workspace", "Part"],
            "nodes": [
                { "i": 1, "p": 0, "c": 1, "n": "Baseplate" },
                { "i": 2, "p": 1, "c": 2, "n": "Workspace" },
                { "i": 3, "p": 2, "c": 3, "n": "My Part" },
                { "i": 4, "p": 2, "c": 3, "n": "end" },
                { "i": 5, "p": 2, "c": 3, "n": "2Fast" },
                { "i": 6, "p": 2, "c": 3, "n": "" },
                { "i": 7, "p": 2, "c": 3, "n": "Parent" },
                { "i": 8, "p": 2, "c": 3, "n": "Ok" },
            ],
        }));

        assert_eq!(session.apply(&tree), Answer::Ok);

        let text = definitions(&session);

        parses(&text);

        assert!(text.contains("\tOk: Part\n"), "{text}");
        assert!(!text.contains("My Part"), "{text}");
        assert!(!text.contains("2Fast"), "{text}");
        assert!(!text.contains("end:"), "{text}");
        assert!(!text.contains("Parent:"), "{text}");
    }

    /// One field cannot hold two types, so the first child of a name wins
    #[test]
    fn two_children_with_one_name_write_one_field() {
        let mut session = opened();

        let tree = decode(json!({
            "kind": "full", "session": ID, "seq": 2, "chunk": 1, "final": true, "root": 1,
            "classes": ["DataModel", "Workspace", "Part", "Folder"],
            "nodes": [
                { "i": 1, "p": 0, "c": 1, "n": "Baseplate" },
                { "i": 2, "p": 1, "c": 2, "n": "Workspace" },
                { "i": 3, "p": 2, "c": 3, "n": "Twin" },
                { "i": 4, "p": 2, "c": 4, "n": "Twin" },
            ],
        }));

        assert_eq!(session.apply(&tree), Answer::Ok);

        let text = definitions(&session);

        parses(&text);

        assert_eq!(text.matches("\tTwin: ").count(), 1, "{text}");
        assert!(
            text.contains("\tTwin: Part\n"),
            "the first child wins: {text}"
        );
    }

    /// A class the mirror cannot spell as a type falls back to `Instance`
    #[test]
    fn a_class_that_is_not_a_type_name_falls_back() {
        let mut session = opened();

        let tree = decode(json!({
            "kind": "full", "session": ID, "seq": 2, "chunk": 1, "final": true, "root": 1,
            "classes": ["DataModel", "Workspace", "not a class"],
            "nodes": [
                { "i": 1, "p": 0, "c": 1, "n": "Baseplate" },
                { "i": 2, "p": 1, "c": 2, "n": "Workspace" },
                { "i": 3, "p": 2, "c": 3, "n": "Odd" },
            ],
        }));

        assert_eq!(session.apply(&tree), Answer::Ok);

        let text = definitions(&session);

        parses(&text);

        assert!(text.contains("\tOdd: Instance\n"), "{text}");
    }

    #[test]
    fn an_empty_session_writes_nothing() {
        let session = Session::new(ID.to_owned());

        assert!(definitions(&session).is_empty());
    }

    /// A root with no field of its own gives a reader nothing
    #[test]
    fn a_root_without_a_usable_child_writes_nothing() {
        let mut session = opened();

        let tree = decode(json!({
            "kind": "full", "session": ID, "seq": 2, "chunk": 1, "final": true, "root": 1,
            "classes": ["DataModel", "Workspace"],
            "nodes": [
                { "i": 1, "p": 0, "c": 1, "n": "Baseplate" },
                { "i": 2, "p": 1, "c": 2, "n": "not a name" },
            ],
        }));

        assert_eq!(session.apply(&tree), Answer::Ok);
        assert!(definitions(&session).is_empty());
    }

    /*
    A second text declares no name the first one used.

    The analyzer holds one global scope for the life of the process, and a
    second declaration of one type name fails the file that carries it.
    */
    #[test]
    fn a_later_text_declares_fresh_names() {
        let mut session = place();
        let first = definitions(&session);

        let delta = decode(json!({
            "kind": "delta", "session": ID, "seq": 3,
            "renamed": [{ "i": 3, "n": "Ground" }],
        }));

        assert_eq!(session.apply(&delta), Answer::Ok);

        let second = definitions(&session);
        let name = first
            .split_whitespace()
            .find(|word| word.starts_with("_larvae_"))
            .expect("the first text declares a type");

        assert!(!second.contains(name), "{name} is in both texts");
        assert!(second.contains("\tGround: Part\n"), "{second}");
    }

    /// A reload keeps the names fresh, because the analyzer keeps the old scope
    #[test]
    fn a_reset_keeps_the_names_fresh() {
        let mut session = place();
        let before = session.revision();

        assert_eq!(session.apply(&hello(1)), Answer::Ok);
        assert!(session.revision() > before);
    }
}
