/*!
Goto definition, references, highlights and rename, inside one document.

The four requests answer one question: which binding does the cursor name,
and where else does the file name it. The scope resolver in
[`crate::lint::scope`] already answers that question for the lints, so this
module holds no scope walk of its own. It turns a byte offset into a token, a
token into a binding, and a binding back into byte ranges.

The scope is one file. A local, a parameter and a loop variable resolve. A
global does not, because the file that declares it can be any other file in
the project, and larvae keeps no cross file index yet. Every function gives
the empty answer for a global instead of a wrong one.

The resolver records two kinds of read. A read of a plain name carries the
token that reads it. A read inside a type annotation or inside a backtick
string carries a byte offset instead, because the tree keeps a type as an
uninterpreted span and the lexer keeps a backtick string as one token. Only
the first kind can become a range, so this module reads the exact ones from
`Names::read_of`. [`rename`] refuses a binding that has any of the second
kind, because a rename that misses one use produces code that does not run.
*/

use crate::lint::scope::Names;
use crate::syntax::lexer::{Tok, TokKind};

/// What a document highlight means, in the numbering of the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `DocumentHighlightKind.Read`
    Read = 2,
    /// `DocumentHighlightKind.Write`. The declaration counts as a write.
    Write = 3,
}

impl Kind {
    /// The number that the protocol carries.
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// The declaration of the binding under the cursor, as a byte range.
pub fn definition(src: &str, byte: u32) -> Option<(u32, u32)> {
    target_at(src, byte)?.declaration
}

/// Every use of the binding under the cursor, as byte ranges, in file order.
pub fn references(src: &str, byte: u32, include_declaration: bool) -> Vec<(u32, u32)> {
    match target_at(src, byte) {
        Some(target) => target.ranges(include_declaration),

        None => Vec::new(),
    }
}

/*
The same set as [`references`], with the role of each use.

The editor paints a write in a different colour from a read, so the author
sees where the value changes. The declaration is a write: it is the first
place that gives the binding a value.
*/
pub fn highlights(src: &str, byte: u32) -> Vec<((u32, u32), Kind)> {
    let Some(target) = target_at(src, byte) else {
        return Vec::new();
    };

    let mut out: Vec<((u32, u32), Kind)> = Vec::new();

    out.extend(target.declaration.map(|r| (r, Kind::Write)));
    out.extend(target.writes.iter().map(|&r| (r, Kind::Write)));
    out.extend(target.reads.iter().map(|&r| (r, Kind::Read)));

    out.sort_unstable_by_key(|&(range, _)| range);
    out.dedup_by_key(|&mut (range, _)| range);

    out
}

/*
The byte ranges that a rename must replace, or `None` if it cannot run.

The function refuses more than it accepts, and each refusal keeps a file that
runs from becoming a file that does not:

- The cursor names a global or an undefined name. The declaration can be in
  another file, so a rename here changes one file out of several.
- The cursor names the implicit `self` of a method. It has no name token to
  replace.
- The binding has a use that carries no token, see the module comment. A
  partial rename leaves the old name behind and the file stops running.
- The new name is not a Luau identifier, or it is a reserved word. The name
  test lives in [`crate::rules::native::is_ident`], which the transformer
  already uses for the same question.
*/
pub fn rename(src: &str, byte: u32, new_name: &str) -> Option<Vec<(u32, u32)>> {
    if !crate::rules::native::is_ident(new_name) {
        return None;
    }

    let target = target_at(src, byte)?;

    target.declaration?;

    if target.approximate {
        return None;
    }

    Some(target.ranges(true))
}

/// Everywhere that one binding appears, as byte ranges.
struct Target {
    /// `None` when the declaration has no name token of its own, as `self` does.
    declaration: Option<(u32, u32)>,
    reads: Vec<(u32, u32)>,
    writes: Vec<(u32, u32)>,
    /// True when the binding has a use that carries no token, so no range covers it.
    approximate: bool,
}

impl Target {
    fn ranges(&self, include_declaration: bool) -> Vec<(u32, u32)> {
        let mut out: Vec<(u32, u32)> = Vec::new();

        if include_declaration {
            out.extend(self.declaration);
        }

        out.extend(self.reads.iter().copied());
        out.extend(self.writes.iter().copied());

        out.sort_unstable();
        out.dedup();

        out
    }
}

/*
Resolve the cursor to a binding, and collect its uses.

Each request parses the document again. A parse of one file takes
microseconds, and the server holds no cache of a resolved file, so a shared
parse would need one. The cost is not visible between keystrokes.

A file that does not parse gives `None`. The editor asks for a definition
while the author is in the middle of a line, so a half written file is the
normal case and not the error case.
*/
fn target_at(src: &str, byte: u32) -> Option<Target> {
    let lexed = crate::syntax::lexer::lex(src).ok()?;
    let chunk = crate::syntax::parser::parse(src, &lexed.toks).ok()?;
    let toks = &lexed.toks;

    let cursor = token_at(toks, byte)?;

    // Only a name can be a binding. A keyword and an operator stop here.
    if toks[cursor as usize].kind != TokKind::Ident {
        return None;
    }

    let names = crate::lint::scope::resolve(src, toks, &chunk);
    let index = binding_of(&names, cursor)?;
    let binding = &names.bindings[index];

    let range = |token: u32| {
        let tok = toks[token as usize];

        (tok.start, tok.end)
    };

    /*
    The reads come from `read_of` and not from `binding.reads`.

    `read_of` holds only the reads that a name token made, and it maps each
    one to the binding that the scope walk chose. `binding.reads` mixes those
    token indexes with the byte offsets of the approximate reads, and nothing
    tells the two apart afterwards.
    */
    let mut reads: Vec<(u32, u32)> = names
        .read_of
        .iter()
        .filter(|&(_, &b)| b == index)
        .map(|(&token, _)| range(token))
        .collect();

    reads.sort_unstable();

    let mut writes: Vec<(u32, u32)> = binding.writes.iter().map(|&t| range(t)).collect();
    writes.sort_unstable();

    /*
    A method's implicit `self` is declared at the first token of the body,
    because it has no name of its own. The text of that token is `(` and not
    the name, and that difference is the test.
    */
    let declared = toks[binding.declared_at as usize];
    let declaration = (declared.text(src) == binding.name).then(|| range(binding.declared_at));

    Some(Target {
        declaration,
        approximate: binding.reads.len() > reads.len(),
        reads,
        writes,
    })
}

/*
The binding that one name token refers to.

The three lookups cover the three ways a name can appear. `by_token` holds
the declarations, `read_of` holds the reads, and the writes stay on the
binding itself. The walk built all three, so a name inside a shadowed scope
already points at the binding that the scope rules chose. A match on the text
of the name would point at the wrong one.
*/
fn binding_of(names: &Names, token: u32) -> Option<usize> {
    if let Some(&index) = names.by_token.get(&token) {
        return Some(index);
    }

    if let Some(&index) = names.read_of.get(&token) {
        return Some(index);
    }

    names
        .bindings
        .iter()
        .position(|b| b.writes.contains(&token))
}

/*
The token that holds a byte offset.

The editor sends the position of the caret, and a caret sits between two
characters. So a caret directly after a name gives the byte of the next
token, and a search for the token that contains the byte finds the `)` of
`print(x)`. The function then steps left onto a name that ends at the same
byte. Without that step, goto definition fails on the name the author just
finished typing.

A byte inside the whitespace between two tokens belongs to neither, and gives
`None`.
*/
fn token_at(toks: &[Tok], byte: u32) -> Option<u32> {
    let after = toks.partition_point(|t| t.start <= byte);
    let index = after.checked_sub(1)?;

    let holds = byte < toks[index].end;

    if holds && toks[index].kind == TokKind::Ident {
        return Some(index as u32);
    }

    // the caret touches the left edge of this token, so a name on the left wins
    if toks[index].start == byte
        && let Some(before) = index.checked_sub(1)
        && toks[before].end == byte
        && toks[before].kind == TokKind::Ident
    {
        return Some(before as u32);
    }

    if holds || byte == toks[index].end {
        return Some(index as u32);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The byte offset of the `nth` occurrence of `needle`, counting from zero.
    fn at(src: &str, needle: &str, nth: usize) -> u32 {
        src.match_indices(needle)
            .nth(nth)
            .map(|(i, _)| i as u32)
            .expect("the needle is in the source")
    }

    fn text(src: &str, range: (u32, u32)) -> &str {
        &src[range.0 as usize..range.1 as usize]
    }

    // --- the cursor lookup -------------------------------------------------

    #[test]
    fn a_byte_inside_a_token_finds_it() {
        let src = "local abc = 1\n";
        let lexed = crate::syntax::lexer::lex(src).expect("lexes");

        let found = token_at(&lexed.toks, 7).expect("a token");

        assert_eq!(lexed.toks[found as usize].text(src), "abc");
    }

    #[test]
    fn the_first_byte_of_a_token_finds_it() {
        let src = "local abc = 1\n";
        let lexed = crate::syntax::lexer::lex(src).expect("lexes");

        let found = token_at(&lexed.toks, 6).expect("a token");

        assert_eq!(lexed.toks[found as usize].text(src), "abc");
    }

    /// A caret after the last letter of a name still names it.
    #[test]
    fn the_byte_just_past_a_token_finds_it() {
        let src = "local abc = 1\n";
        let lexed = crate::syntax::lexer::lex(src).expect("lexes");

        let found = token_at(&lexed.toks, 9).expect("a token");

        assert_eq!(lexed.toks[found as usize].text(src), "abc");
    }

    /// The case that the step to the left exists for.
    #[test]
    fn a_caret_between_a_name_and_a_bracket_names_the_name() {
        let src = "print(abc)\n";
        let lexed = crate::syntax::lexer::lex(src).expect("lexes");

        let found = token_at(&lexed.toks, at(src, ")", 0)).expect("a token");

        assert_eq!(lexed.toks[found as usize].text(src), "abc");
    }

    #[test]
    fn a_byte_in_the_whitespace_finds_nothing() {
        // two spaces, so one byte of the gap touches neither token
        let src = "local abc  = 1\n";
        let lexed = crate::syntax::lexer::lex(src).expect("lexes");

        assert_eq!(token_at(&lexed.toks, 9), Some(1), "the end of `abc`");
        assert_eq!(token_at(&lexed.toks, 10), None, "the gap before `=`");
        assert_eq!(token_at(&lexed.toks, 11), Some(2), "the start of `=`");
    }

    #[test]
    fn a_byte_past_the_last_token_finds_nothing() {
        let src = "local abc = 1\n";
        let lexed = crate::syntax::lexer::lex(src).expect("lexes");

        assert_eq!(token_at(&lexed.toks, 40), None);
    }

    // --- definition --------------------------------------------------------

    #[test]
    fn a_read_finds_its_declaration() {
        let src = "local value = 1\nprint(value)\n";
        let found = definition(src, at(src, "value", 1)).expect("a declaration");

        assert_eq!(found, (6, 11));
        assert_eq!(text(src, found), "value");
    }

    #[test]
    fn the_declaration_finds_itself() {
        let src = "local value = 1\nprint(value)\n";

        assert_eq!(definition(src, at(src, "value", 0)), Some((6, 11)));
    }

    #[test]
    fn a_write_finds_the_declaration() {
        let src = "local value = 1\nvalue = 2\n";

        assert_eq!(definition(src, at(src, "value", 1)), Some((6, 11)));
    }

    #[test]
    fn a_parameter_is_a_declaration() {
        let src = "local function f(arg)\n\treturn arg\nend\n";
        let found = definition(src, at(src, "arg", 1)).expect("a declaration");

        assert_eq!(found.0, at(src, "arg", 0));
    }

    /// The reason the lookup goes through the resolver and not through the text.
    #[test]
    fn a_shadowed_name_finds_the_binding_that_holds_it() {
        let src = "local x = 1\ndo\n\tlocal x = 2\n\tprint(x)\nend\nprint(x)\n";

        let outer = at(src, "x", 0);
        let inner = at(src, "x", 1);

        assert_eq!(
            definition(src, at(src, "x", 2)),
            Some((inner, inner + 1)),
            "the read inside the block sees the inner x"
        );

        assert_eq!(
            definition(src, at(src, "x", 3)),
            Some((outer, outer + 1)),
            "the read after the block sees the outer x"
        );
    }

    /// Lua evaluates the values before the names exist.
    #[test]
    fn a_local_that_reads_its_own_name_finds_the_outer_one() {
        let src = "local x = 1\ndo\n\tlocal x = x\nend\n";

        assert_eq!(definition(src, at(src, "x", 2)), Some((6, 7)));
    }

    #[test]
    fn a_global_has_no_declaration() {
        let src = "print(other)\n";

        assert_eq!(definition(src, at(src, "print", 0)), None);
        assert_eq!(definition(src, at(src, "other", 0)), None);
    }

    /// A method's `self` binds, but no token spells it out.
    #[test]
    fn the_implicit_self_has_no_declaration() {
        let src = "function t:m()\n\treturn self.x\nend\n";

        assert_eq!(definition(src, at(src, "self", 0)), None);
        assert_eq!(rename(src, at(src, "self", 0), "this"), None);
    }

    #[test]
    fn a_file_that_does_not_parse_answers_nothing() {
        let src = "local x = = 1\nprint(x)\n";

        assert_eq!(definition(src, at(src, "x", 1)), None);
        assert!(references(src, at(src, "x", 1), true).is_empty());
        assert!(highlights(src, at(src, "x", 1)).is_empty());
        assert_eq!(rename(src, at(src, "x", 1), "y"), None);
    }

    #[test]
    fn an_empty_file_answers_nothing() {
        assert_eq!(definition("", 0), None);
        assert!(references("", 0, true).is_empty());
    }

    // --- references --------------------------------------------------------

    #[test]
    fn every_use_is_a_reference() {
        let src = "local value = 1\nvalue = 2\nprint(value)\n";
        let found = references(src, at(src, "value", 2), true);

        assert_eq!(
            found,
            vec![
                (at(src, "value", 0), at(src, "value", 0) + 5),
                (at(src, "value", 1), at(src, "value", 1) + 5),
                (at(src, "value", 2), at(src, "value", 2) + 5),
            ]
        );
    }

    #[test]
    fn the_declaration_can_be_left_out() {
        let src = "local value = 1\nvalue = 2\nprint(value)\n";
        let found = references(src, at(src, "value", 0), false);

        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|&r| r.0 != at(src, "value", 0)));
    }

    #[test]
    fn a_reference_search_starts_from_the_declaration_too() {
        let src = "local value = 1\nprint(value)\nprint(value)\n";

        assert_eq!(references(src, at(src, "value", 0), true).len(), 3);
    }

    #[test]
    fn a_shadowed_binding_keeps_its_own_references() {
        let src = "local x = 1\ndo\n\tlocal x = 2\n\tprint(x)\nend\nprint(x)\n";

        let inner = references(src, at(src, "x", 1), true);
        let outer = references(src, at(src, "x", 3), true);

        assert_eq!(
            inner,
            vec![
                (at(src, "x", 1), at(src, "x", 1) + 1),
                (at(src, "x", 2), at(src, "x", 2) + 1),
            ]
        );

        assert_eq!(
            outer,
            vec![
                (at(src, "x", 0), at(src, "x", 0) + 1),
                (at(src, "x", 3), at(src, "x", 3) + 1),
            ]
        );
    }

    #[test]
    fn a_global_has_no_references() {
        let src = "counter = 1\nprint(counter)\n";

        assert!(references(src, at(src, "counter", 0), true).is_empty());
        assert!(references(src, at(src, "counter", 1), true).is_empty());
    }

    #[test]
    fn a_field_of_the_same_name_is_not_a_reference() {
        let src = "local name = 1\nlocal t = {}\nprint(t.name, name)\n";
        let found = references(src, at(src, "name", 0), true);

        assert_eq!(found.len(), 2, "the declaration and the last read");
        assert_eq!(found[1].0, at(src, "name", 2));
    }

    // --- highlights --------------------------------------------------------

    #[test]
    fn a_write_and_a_read_carry_different_kinds() {
        let src = "local value = 1\nvalue = 2\nprint(value)\n";
        let found = highlights(src, at(src, "value", 2));

        assert_eq!(
            found,
            vec![
                ((at(src, "value", 0), at(src, "value", 0) + 5), Kind::Write),
                ((at(src, "value", 1), at(src, "value", 1) + 5), Kind::Write),
                ((at(src, "value", 2), at(src, "value", 2) + 5), Kind::Read),
            ]
        );
    }

    #[test]
    fn the_kinds_carry_the_numbers_of_the_protocol() {
        assert_eq!(Kind::Read.code(), 2);
        assert_eq!(Kind::Write.code(), 3);
    }

    // --- rename ------------------------------------------------------------

    #[test]
    fn a_rename_covers_every_use() {
        let src = "local value = 1\nvalue = 2\nprint(value)\n";
        let found = rename(src, at(src, "value", 2), "total").expect("a rename");

        assert_eq!(found.len(), 3);
        assert!(found.iter().all(|&r| text(src, r) == "value"));
    }

    #[test]
    fn a_rename_starts_from_the_declaration_too() {
        let src = "local value = 1\nprint(value)\n";

        assert_eq!(
            rename(src, at(src, "value", 0), "total").map(|r| r.len()),
            Some(2)
        );
    }

    #[test]
    fn a_rename_of_a_shadowed_binding_leaves_the_other_alone() {
        let src = "local x = 1\ndo\n\tlocal x = 2\n\tprint(x)\nend\nprint(x)\n";
        let found = rename(src, at(src, "x", 2), "inner").expect("a rename");

        assert_eq!(
            found,
            vec![
                (at(src, "x", 1), at(src, "x", 1) + 1),
                (at(src, "x", 2), at(src, "x", 2) + 1),
            ]
        );
    }

    #[test]
    fn a_rename_needs_a_local() {
        let src = "print(other)\n";

        assert_eq!(rename(src, at(src, "other", 0), "value"), None);
        assert_eq!(rename(src, at(src, "print", 0), "show"), None);
    }

    #[test]
    fn a_rename_needs_a_valid_identifier() {
        let src = "local value = 1\nprint(value)\n";
        let cursor = at(src, "value", 0);

        assert_eq!(rename(src, cursor, ""), None);
        assert_eq!(rename(src, cursor, "2fast"), None);
        assert_eq!(rename(src, cursor, "with space"), None);
        assert_eq!(rename(src, cursor, "a-b"), None);
        assert_eq!(rename(src, cursor, "t.k"), None);
    }

    #[test]
    fn a_rename_refuses_a_reserved_word() {
        let src = "local value = 1\nprint(value)\n";
        let cursor = at(src, "value", 0);

        for word in ["end", "local", "function", "nil", "then", "until"] {
            assert_eq!(rename(src, cursor, word), None, "{word} is reserved");
        }

        assert!(
            rename(src, cursor, "_end").is_some(),
            "but `_end` is a name"
        );
    }

    /*
    The lexer keeps a backtick string whole, so the read inside it has no
    token and no range. A rename would leave that use behind, so it refuses.
    */
    #[test]
    fn a_rename_refuses_a_binding_that_a_string_reads() {
        let src = "local who = \"world\"\nprint(`hello {who}`)\n";

        assert_eq!(rename(src, at(src, "who", 0), "name"), None);
    }

    /// The tree keeps a type as an uninterpreted span, so the same rule holds.
    #[test]
    fn a_rename_refuses_a_binding_that_a_type_reads() {
        let src = "local T = require(\"./t\")\ntype Foo = T.Foo\nprint(T)\n";

        assert_eq!(rename(src, at(src, "T", 0), "Types"), None);
    }
}
