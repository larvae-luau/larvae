/*!
Obfuscation, checked against Luau itself.

Two claims need proof and neither is provable by reading the output. The
file has to still parse, so the whole conformance corpus goes through the
obfuscator and back through the parser. And every string has to hold the
same bytes, so a table of literals runs in the Luau VM before and after,
and the two answers are compared byte for byte.
*/

use larvae::obfuscate::obfuscate;
use larvae::syntax::{lexer, parser};

/// One line, which is what the pipeline asks for under `obfuscate`.
fn run(src: &str) -> String {
    obfuscate(src, usize::MAX).expect("obfuscates")
}

fn parses(src: &str) -> Result<(), String> {
    let lexed = lexer::lex(src).map_err(|e| e.message)?;
    parser::parse(src, &lexed.toks).map_err(|e| e.message)?;

    Ok(())
}

/// The strings a chunk returns, as raw bytes.
fn strings_of(source: &str) -> Result<Vec<Vec<u8>>, String> {
    let lua = mlua::Lua::new();
    let table: mlua::Table = lua.load(source).eval().map_err(|e| e.to_string())?;
    let mut out = Vec::new();

    for value in table.sequence_values::<mlua::LuaString>() {
        out.push(value.map_err(|e| e.to_string())?.as_bytes().to_vec());
    }

    Ok(out)
}

/// Every literal form Luau accepts, in one chunk the VM can return.
const LITERALS: &str = r#"
local prefix = "p"
return {
	"hi",
	'single',
	"",
	"tab\there",
	"quote\"inside",
	"back\\slash",
	"\65\066\9",
	"\x41\x7f\xff",
	"\u{48}\u{20AC}\u{1F600}",
	"\u{D800}",
	"unknown \q escape",
	"wrapped\
line",
	"skipped\z
	   whitespace",
	"utf8 é ü 漢",
	[[long bracket]],
	[[
first newline goes]],
	[==[level two ]] inside]==],
	prefix .. "joined",
}
"#;

#[test]
fn every_string_holds_the_same_bytes_after_obfuscation() {
    let before = strings_of(LITERALS).expect("the fixture runs");
    let after = strings_of(&run(LITERALS)).expect("the obfuscated fixture runs");

    assert_eq!(before.len(), after.len(), "a literal went missing");
    assert_eq!(before, after);
}

/// Every literal that the obfuscator can read comes out escaped. The escape is
/// the point: a reader of the output sees no words.
#[test]
fn the_readable_text_of_a_string_is_gone() {
    let out = run("return { \"a secret\", [[another]] }\n");

    assert!(!out.contains("secret"), "{out}");
    assert!(!out.contains("another"), "{out}");
    assert!(out.contains("\\x73\\x65\\x63"), "{out}");
}

#[test]
fn every_conformance_file_parses_after_obfuscation() {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/parser/luau-conformance"
    );

    let mut checked = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for entry in std::fs::read_dir(dir).expect("the corpus ships with the repo") {
        let path = entry.expect("reads").path();

        if path.extension().and_then(|e| e.to_str()) != Some("luau") {
            continue;
        }

        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(&path).expect("reads");
        let (src, _) = larvae::sys::utf8_stand_in(bytes);

        match obfuscate(&src, usize::MAX) {
            Ok(out) => {
                if let Err(e) = parses(&out) {
                    failures.push(format!("{name}: the output does not parse, {e}"));
                }
            }

            Err(e) => failures.push(format!("{name}: {e}")),
        }

        checked += 1;
    }

    assert!(checked > 0, "the corpus is empty");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The token stream must keep its shape: obfuscation moves names and string
/// bytes, and nothing else.
#[test]
fn the_program_keeps_its_structure() {
    let src = "local function add(a, b)\n\treturn a + b\nend\n\nreturn add(1, 2)\n";
    let out = run(src);

    let before = lexer::lex(src).unwrap().toks;
    let after = lexer::lex(&out).unwrap().toks;

    assert_eq!(before.len(), after.len(), "{out}");

    for (a, b) in before.iter().zip(&after) {
        assert_eq!(
            std::mem::discriminant(&a.kind),
            std::mem::discriminant(&b.kind),
            "{out}"
        );
    }
}
