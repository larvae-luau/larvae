/*!
The edit model

Every transform in larvae says what it wants as a byte range replacement
against the original source. Bytes nobody touched are copied straight
through, which is what makes retain-lines free and keeps a whole file close
to memcpy speed

Edits remember which rule made them. Two rules reaching for the same bytes
is a real thing, ex: one rewriting a method head while another inserts a
self parameter into it. Interleaving both would emit something neither rule
meant, so the first one stands and the second is reported instead of
vanishing
*/

/// Byte range replacement, start, end, replacement text
pub type Edit = (u32, u32, String);

/*
Which bucket a rule reports under when the run finishes

Native means the rule ships inside larvae, which is all of them today.
The split that matters later is against transforms a user writes in their
own repo, so the summary can say where a change came from once extensions
land rather than lumping everything together
*/
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Family {
    /// The require rewriter, it gets its own line in the summary
    Requires,
    /// Built into larvae, both our own rules and the darklua parity set
    Native,
    /// A transform loaded from the user's repo, lands with extensions in M3
    Extension,
}

/// A rule, and the bucket it reports under
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Rule {
    pub name: &'static str,
    pub family: Family,
}

/// Two edits wanted the same bytes, the later one never reached the output
pub struct Conflict {
    /// Byte offset the collision starts at
    pub at: u32,
    pub kept: &'static str,
    pub dropped: &'static str,
}

/// One file's edits, each tagged with the rule behind it
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
    Everything pushed inside `f` reports under this family, so a whole
    module's worth of rules gets bucketed at the one place that dispatches
    them instead of every rule repeating it
    */
    pub fn family(&mut self, family: Family, f: impl FnOnce(&mut Self)) {
        let previous = std::mem::replace(&mut self.current, family);
        f(self);
        self.current = previous;
    }

    /// Add one edit
    pub fn push(&mut self, name: &'static str, edit: Edit) {
        self.items.push(edit);
        self.owners.push(Rule {
            name,
            family: self.current,
        });
    }

    /*
    Run a rule against a plain vec and label whatever it pushed, rules stay
    free of bookkeeping and still come out traceable
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

    /// Rules that actually changed something, each listed once
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
Decide the order edits apply in and drop the ones that collide

Sorting indexes instead of the edits themselves keeps the replacement
strings still, a file with a few thousand edits would otherwise memmove
every one of them
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
An edit that lands inside a deletion is moot, ex: one rule dropping a whole
unreachable statement while another wanted to fold a sum inside it. Those
bytes are leaving either way so there is nothing to report. Anything else,
a partial overlap or a loser buried inside a rewrite, means a rule the user
turned on did not happen and that is worth saying out loud
*/
fn subsumed(winner: &Edit, loser: &Edit) -> bool {
    loser.0 >= winner.0 && loser.1 <= winner.1 && winner.2.bytes().all(|b| b == b'\n')
}

/// Which edits collided, without paying to build the output text
pub fn conflicts(edits: &Edits) -> Vec<Conflict> {
    let mut found = Vec::new();
    plan(edits, &mut found);

    found
}

/// Apply the edits to `src`, collecting anything that collided
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
        // the bytes rule_b wanted are being removed anyway, nothing was lost
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
        // rule_a replaced the span with new text, so rule_b really did not run
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
