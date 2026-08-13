/*!
The edit model

Each transform in larvae expresses its change as a byte range replacement
against the original source. Larvae copies unchanged bytes directly to the
output. This makes retain-lines free and keeps the speed for a whole file
close to memcpy speed.

Each edit records the rule that made it. Two rules can select the same
bytes. For example, one rule rewrites a method head while another rule
inserts a self parameter into it. If larvae interleaved both edits, the
output would match the intent of neither rule. For this reason, larvae
applies the first edit and reports the second edit.
*/

/// A byte range replacement: start, end, and replacement text.
pub type Edit = (u32, u32, String);

/*
The bucket that a rule reports under when the run finishes.

Native means that the rule ships inside larvae. Today all rules are
native. Later, a user can write transforms in their own repo. When
extensions land, this split lets the summary show the source of each
change.
*/
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Family {
    /// The require rewriter. The summary gives it its own line.
    Requires,
    /// Rules built into larvae. This includes the larvae rules and the darklua parity set.
    Native,
    /// A transform loaded from the user's repo. This family lands with extensions in M3.
    Extension,
}

/// A rule and the bucket that it reports under.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Rule {
    pub name: &'static str,
    pub family: Family,
}

/// Two edits selected the same bytes. The later edit did not reach the output.
pub struct Conflict {
    /// The byte offset where the collision starts.
    pub at: u32,
    pub kept: &'static str,
    pub dropped: &'static str,
}

/// The edits for one file. Each edit has a tag with the rule that made it.
pub struct Edits {
    items: Vec<Edit>,
    owners: Vec<Rule>,
    current: Family,
}

impl Default for Edits {
    fn default() -> Self {
        Self::new()
    }
}

impl Edits {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            owners: Vec::new(),
            current: Family::Requires,
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /*
    Each edit pushed inside `f` reports under this family. The one place
    that dispatches a module's rules sets the bucket once. The rules do
    not repeat the bucket.
    */
    pub fn family(&mut self, family: Family, f: impl FnOnce(&mut Self)) {
        let previous = std::mem::replace(&mut self.current, family);
        f(self);
        self.current = previous;
    }

    /// Add one edit.
    pub fn push(&mut self, name: &'static str, edit: Edit) {
        self.items.push(edit);
        self.owners.push(Rule {
            name,
            family: self.current,
        });
    }

    /*
    Run a rule against a plain vec, then label each edit that it pushed.
    The rules stay free of bookkeeping, but each edit stays traceable.
    */
    pub fn run(&mut self, name: &'static str, f: impl FnOnce(&mut Vec<Edit>)) {
        f(&mut self.items);
        self.owners.resize(
            self.items.len(),
            Rule {
                name,
                family: self.current,
            },
        );
    }

    /// The rules that changed something. Each rule appears once.
    pub fn applied(&self) -> Vec<Rule> {
        let mut out: Vec<Rule> = Vec::new();

        for &owner in &self.owners {
            if !out.contains(&owner) {
                out.push(owner);
            }
        }

        out
    }
}

/*
Decide the order in which the edits apply, and drop the edits that
collide.

Larvae sorts indexes and not the edits themselves. This keeps the
replacement strings in place. A sort of the edits would memmove each
string in a file with a few thousand edits.
*/
fn plan(edits: &Edits, conflicts: &mut Vec<Conflict>) -> Vec<u32> {
    let mut order: Vec<u32> = (0..edits.items.len() as u32).collect();
    order.sort_by_key(|&i| {
        let e = &edits.items[i as usize];

        (e.0, e.1)
    });

    let mut keep = Vec::with_capacity(order.len());
    let mut cursor = 0u32;
    let mut winner: Option<u32> = None;

    for i in order {
        let (start, end, _) = &edits.items[i as usize];

        if *start < cursor {
            let w = winner.expect("cursor only moves after an edit lands");

            if !subsumed(&edits.items[w as usize], &edits.items[i as usize]) {
                conflicts.push(Conflict {
                    at: *start,
                    kept: edits.owners[w as usize].name,
                    dropped: edits.owners[i as usize].name,
                });
            }

            continue;
        }

        keep.push(i);
        cursor = *end;
        winner = Some(i);
    }

    keep
}

/*
An edit that lands inside a deletion has no effect. For example, one rule
removes a whole unreachable statement while another rule folds a sum
inside it. The deletion removes those bytes in each case, so there is
nothing to report. A partial overlap, or a lost edit inside a rewrite, is
different. It means that a rule the user enabled did not run, and larvae
must report that.
*/
fn subsumed(winner: &Edit, loser: &Edit) -> bool {
    loser.0 >= winner.0 && loser.1 <= winner.1 && winner.2.bytes().all(|b| b == b'\n')
}

/// Report the edits that collided. This does not build the output text.
pub fn conflicts(edits: &Edits) -> Vec<Conflict> {
    let mut found = Vec::new();
    plan(edits, &mut found);

    found
}

/// Apply the edits to `src`. Collect each edit that collided.
pub fn splice(src: &str, edits: &Edits, conflicts: &mut Vec<Conflict>) -> String {
    if edits.is_empty() {
        return src.to_owned();
    }

    let order = plan(edits, conflicts);
    let extra: usize = edits.items.iter().map(|e| e.2.len()).sum();
    let mut out = String::with_capacity(src.len() + extra);
    let mut cursor = 0usize;

    for i in order {
        let (start, end, new) = &edits.items[i as usize];

        out.push_str(&src[cursor..*start as usize]);
        out.push_str(new);
        cursor = *end as usize;
    }

    out.push_str(&src[cursor..]);

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_ranges() {
        let src = r#"require("./a") + require("./b")"#;
        let mut edits = Edits::new();
        edits.push("requires", (9, 12, "@game/RS/a".into()));
        edits.push("requires", (26, 29, "@game/RS/b".into()));

        let mut hits = Vec::new();
        let out = splice(src, &edits, &mut hits);

        assert_eq!(out, r#"require("@game/RS/a") + require("@game/RS/b")"#);
        assert!(hits.is_empty());
    }

    #[test]
    fn overlap_keeps_the_first_and_names_both() {
        let src = "local x = 1";
        let mut edits = Edits::new();
        edits.push("rule_a", (0, 7, "const y".into()));
        edits.push("rule_b", (6, 7, "z".into()));

        let mut hits = Vec::new();
        let out = splice(src, &edits, &mut hits);

        assert_eq!(out, "const y = 1");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kept, "rule_a");
        assert_eq!(hits[0].dropped, "rule_b");
    }

    #[test]
    fn an_edit_inside_a_deletion_stays_quiet() {
        // The deletion removes the bytes that rule_b selected, so no change is lost.
        let src = "local x = 2 + 3\n";
        let mut edits = Edits::new();
        edits.push("rule_a", (0, 15, String::new()));
        edits.push("rule_b", (10, 15, "5".into()));

        let mut hits = Vec::new();
        let out = splice(src, &edits, &mut hits);

        assert_eq!(out, "\n");
        assert!(hits.is_empty());
    }

    #[test]
    fn an_edit_inside_a_rewrite_still_warns() {
        // rule_a replaced the span with new text, so rule_b did not run.
        let src = "local x = 2 + 3\n";
        let mut edits = Edits::new();
        edits.push("rule_a", (0, 15, "local x = f(2 + 3)".into()));
        edits.push("rule_b", (10, 15, "5".into()));

        let mut hits = Vec::new();
        splice(src, &edits, &mut hits);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].dropped, "rule_b");
    }

    #[test]
    fn insertions_at_one_point_all_land() {
        let src = "return 1\n";
        let mut edits = Edits::new();
        edits.push("add_luau_directive", (0, 0, "--!strict\n".into()));
        edits.push("append_text_comment", (0, 0, "-- generated\n".into()));

        let mut hits = Vec::new();
        let out = splice(src, &edits, &mut hits);

        assert_eq!(out, "--!strict\n-- generated\nreturn 1\n");
        assert!(hits.is_empty());
    }

    #[test]
    fn conflicts_agrees_with_splice() {
        let mut edits = Edits::new();
        edits.push("rule_a", (0, 5, "aaaa".into()));
        edits.push("rule_b", (2, 8, "bbbb".into()));

        let mut from_splice = Vec::new();
        splice("0123456789", &edits, &mut from_splice);

        assert_eq!(conflicts(&edits).len(), from_splice.len());
    }
}
