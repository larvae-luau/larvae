/*!
These tests check parser conformance. Every snippet must parse, must tile the
token stream with no holes, and must print back byte for byte. That set of
three checks is the M1a exit criterion.
*/

use larvae::syntax::{lexer, parser, printer};

/// This function parses, checks coverage, prints, and compares in one step.
#[track_caller]
fn round_trip(src: &str) {
    let lexed = match lexer::lex(src) {
        Ok(l) => l,

        Err(e) => panic!("lex error at {}: {}\nsource:\n{src}", e.offset, e.message),
    };

    let chunk = match parser::parse(src, &lexed.toks) {
        Ok(c) => c,

        Err(e) => {
            let upto = &src[..e.offset.min(src.len())];
            let line = upto.matches('\n').count() + 1;

            panic!(
                "parse error at line {line} (byte {}): {}\nsource:\n{src}",
                e.offset, e.message
            );
        }
    };

    let holes = printer::coverage_errors(&chunk);
    assert!(holes.is_empty(), "coverage holes {holes:?}\nsource:\n{src}");
    let out = printer::print_chunk(src, &lexed.toks, &chunk);
    assert_eq!(out, src, "round trip differed\nsource:\n{src}");
}

#[track_caller]
fn rejects(src: &str) {
    let Ok(lexed) = lexer::lex(src) else { return };
    assert!(
        parser::parse(src, &lexed.toks).is_err(),
        "expected a parse error for:\n{src}"
    );
}

const CORPUS: &[&str] = &[
    // --- explicit type instantiation, which the sweeps mutate and truncate ---
    "local a = charm.atom<<number>>()\n",
    "local a = charm.atom<<(number, string)>>()\n",
    "local a = obj:method<<...number>>()\n",
    "local a = f<<Map<string, number>>>()\n",
    // --- basics ---
    "",
    "\n\n",
    "-- just a comment\n",
    "--!strict\nreturn nil\n",
    "local x = 1",
    "local x, y, z = 1, 2, 3\n",
    "local x = 1;\nlocal y = 2;\n",
    ";;;",
    "x = 1",
    "x, y = y, x",
    "a.b.c = 1",
    "a[1][2] = 3",
    "local t = {}\nt.x = 1\n",
    "return",
    "return 1, 2",
    "return;",
    // --- numbers and strings ---
    "local a = 0x1F\nlocal b = 0b1010\nlocal c = 1_000_000\nlocal d = .5\nlocal e = 1e-9\nlocal f = 1.5e+10\n",
    r#"local s = "double" local t = 'single'"#,
    "local s = [[long]]\nlocal t = [==[nested ]] here]==]\n",
    "local s = `interp {value} here`",
    "local s = `nested {`inner {x}`} done`",
    r#"local s = `braces {("}")} ok`"#,
    // --- operators ---
    "local a = 1 + 2 * 3 - 4 / 5 % 6 ^ 7",
    "local a = 1 // 2",
    "local a = -x + #t + not y",
    "local a = 'x' .. 'y' .. 'z'",
    "local a = x < y and y <= z or w ~= v and u == t",
    "local a = 2 ^ 3 ^ 4",
    "x += 1\nx -= 1\nx *= 2\nx /= 2\nx %= 3\nx ^= 2\nx ..= 'a'\nx //= 2\n",
    // --- control flow ---
    "if x then end",
    "if x then y() elseif z then w() else v() end",
    "while true do break end",
    "repeat x() until done",
    "do local x = 1 end",
    "for i = 1, 10 do end",
    "for i = 10, 1, -1 do end",
    "for k, v in pairs(t) do end",
    "for i, v: string in ipairs(t) do end",
    "while true do continue end",
    "for _ = 1, 3 do if x then continue end end",
    // --- functions ---
    "function f() end",
    "function a.b.c() end",
    "function a.b:c() end",
    "local function f() end",
    "local f = function() end",
    "function f(a, b, ...) return ... end",
    "function f(a: number, b: string?): boolean return true end",
    "function f<T>(x: T): T return x end",
    "function f<T, U...>(x: T, ...: U...): (T, U...) return x, ... end",
    "local f = function(...) local a = {...} end",
    "@native function fast() end",
    "@native @deprecated function both() end",
    "@native local function localfast() end",
    // --- calls ---
    "f()",
    "f(1, 2)",
    "f'str'",
    "f[[long]]",
    "f{1, 2}",
    "obj:method()",
    "obj:method 'str'",
    "local x = a.b.c:d(1)(2)[3]",
    "(f or g)()",
    "local x = (a + b).c",
    // --- tables ---
    "local t = {1, 2, 3}",
    "local t = {a = 1, b = 2}",
    "local t = {['key'] = 1, [2] = 'two'}",
    "local t = {1; 2; 3;}",
    "local t = {a = 1, [f()] = 2, 3,}",
    "local t = {nested = {deep = {1}}}",
    // --- types ---
    "local x: number = 1",
    "local x: string? = nil",
    "local x: {number} = {}",
    "local x: {[string]: number} = {}",
    "local x: {name: string, age: number} = t",
    "local x: (number) -> string = f",
    "local x: (a: number, b: string) -> (boolean, number) = f",
    "local x: () -> () = f",
    "local x: number | string = 1",
    "local x: A & B = t",
    "local x: typeof(y) = y",
    "local x: Foo.Bar = t",
    "local x: Foo<Bar, Baz> = t",
    "local x: 'literal' = 'literal'",
    "local x = y :: number",
    "local x = y :: any :: string",
    "type Point = {x: number, y: number}",
    "type Maybe<T> = T?",
    "export type Handler = (Instance) -> ()",
    "type Fn = <T>(T) -> T",
    "type ReadOnly = {read x: number}",
    "type Pack = (string, ...number) -> ...string",
    "local t: {[string]: {nested: boolean}} = {}",
    // --- if expressions ---
    "local x = if c then 1 else 2",
    "local x = if a then 1 elseif b then 2 else 3",
    "return if x then y else z",
    // --- luau const ---
    "const x = 1",
    "const Signal = require('./signal')",
    // --- realistic module shapes ---
    r#"--!strict
local Players = game:GetService("Players")
local Signal = require("@pkg/signal")

local Module = {}
Module.__index = Module

export type Module = typeof(setmetatable({} :: {
    name: string,
    count: number,
}, Module))

function Module.new(name: string): Module
    local self = setmetatable({}, Module)
    self.name = name
    self.count = 0
    return self :: any
end

function Module:increment(by: number?)
    self.count += by or 1
    if self.count > 10 then
        self:reset()
    end
end

function Module:reset()
    self.count = 0
end

return Module
"#,
    r##"
local t = {}
for i = 1, 100 do
    t[#t + 1] = function(...)
        return select("#", ...) > 0 and { ... } or nil
    end
end
return t
"##,
];

/*
This is explicit type instantiation, Luau's turbofish. The argument is a type
or a type pack, so all of these forms are legal. The round trip matters as much
as the parse, because a swallowed span would drop the type arguments from the
output with no report.
*/
#[test]
fn turbofish_type_arguments() {
    round_trip("local a = charm.atom<<number>>()\n");
    round_trip("local a = charm.atom<<(number, string)>>()\n");
    round_trip("local a = charm.atom<<()>>()\n");
    round_trip("local a = charm.atom<<...number>>()\n");
    round_trip("local a = charm.atom<<{ x: number }>>()\n");
    round_trip("local a = f<<number>>()\n");

    // Nested generics. This is why the bracket count must go by depth.
    round_trip("local a = charm.atom<<Map<string, number>>>()\n");
    round_trip("local a = f<<A<B<C>>>>()\n");

    // A method call also takes type arguments.
    round_trip("local a = obj:method<<number>>()\n");
    round_trip("local a = obj:method<<(number, string)>>()\n");

    // The other two call argument forms also take them.
    round_trip("local a = f<<number>>{ 1, 2 }\n");
    round_trip("local a = f<<string>>\"lit\"\n");

    // These calls are chained, so the suffix loop continues afterward.
    round_trip("local a = f<<number>>().field\n");
    round_trip("local a = f<<number>>()<<string>>()\n");
}

/// A single `<` is still a comparison. The parser must not read it as a
/// turbofish.
#[test]
fn comparisons_are_not_turbofish() {
    round_trip("local a = b < c\n");
    round_trip("local a = b < c and c < d\n");
    round_trip("local a = f(b < c)\n");
    round_trip("local a = t[b < c]\n");
    round_trip("if a < b then end\n");

    // This is a generic call in type position. It is not related to the
    // expression form.
    round_trip("local a: Map<string, number> = x\n");
}

/// A turbofish always precedes a call, so a bare turbofish is a real error.
#[test]
fn a_turbofish_without_a_call_is_rejected() {
    rejects("local a = f<<number>>\n");
    rejects("local a = f<<number>> + 1\n");
    rejects("local a = f<<number\n");
}

#[test]
fn corpus_round_trips() {
    for src in CORPUS {
        round_trip(src);
    }
}

#[test]
fn escaped_quote_strings() {
    round_trip("local s = \"escaped \\\" quote\"\n");
    round_trip("local s = 'it\\'s'\n");
}

#[test]
fn rejects_broken_input() {
    rejects("local = 1");
    rejects("if x then");
    rejects("function f(");
    rejects("local x = ");
    rejects("return return");
    rejects("do end end");
    rejects("local x = {");
    rejects("for i = 1 do end");
    rejects("x +");
    rejects("1 = x");
}

#[test]
fn deep_nesting_errors_instead_of_crashing() {
    // A stack overflow here is the darklua bug class that the design removed.
    let deep = format!("local x = {}1{}", "(".repeat(5000), ")".repeat(5000));
    let lexed = lexer::lex(&deep).unwrap();

    assert!(parser::parse(&deep, &lexed.toks).is_err());

    let deep_tables = format!("local t = {}{}", "{".repeat(5000), "}".repeat(5000));
    let lexed = lexer::lex(&deep_tables).unwrap();

    assert!(parser::parse(&deep_tables, &lexed.toks).is_err());
}

/*
This is a simple form of fuzzing. Byte-level mutations of the corpus must
never panic or hang. The real coverage-guided fuzzing lives in fuzz/ for
nightly runs.
*/
#[test]
fn mutations_never_panic() {
    let interesting = br#""'`[]{}()\\
-"#;
    let mut checked = 0usize;

    for src in CORPUS.iter().filter(|s| s.len() < 400) {
        let bytes = src.as_bytes();

        for pos in 0..bytes.len() {
            for &b in interesting {
                let mut m = bytes.to_vec();
                m[pos] = b;
                let Ok(text) = String::from_utf8(m) else {
                    continue;
                };

                if let Ok(lexed) = lexer::lex(&text) {
                    // The parse must stop and must not panic. Each result is
                    // acceptable.
                    let _ = parser::parse(&text, &lexed.toks);
                }

                checked += 1;
            }
        }
    }

    assert!(
        checked > 1000,
        "expected a decent mutation count, got {checked}"
    );
}

#[test]
fn truncations_never_panic() {
    for src in CORPUS {
        for cut in 0..src.len() {
            if !src.is_char_boundary(cut) {
                continue;
            }

            let text = &src[..cut];

            if let Ok(lexed) = lexer::lex(text) {
                let _ = parser::parse(text, &lexed.toks);
            }
        }
    }
}

// --- classes, export by value, and integer literals ----------------------

#[test]
fn a_class_with_fields_and_methods_parses() {
    round_trip(
        "class Point\n\tpublic x: number\n\tpublic y\n\tfunction magnitude(self)\n\t\treturn math.sqrt(self.x * self.x + self.y * self.y)\n\tend\nend\n",
    );
}

#[test]
fn class_forms_all_parse() {
    round_trip("export class Empty\nend\n");
    round_trip("open class Animal\n\tpublic species: string\nend\n");
    round_trip(
        "class Cat extends Animal\n\tfunction speak(self)\n\t\treturn \"meow\"\n\tend\nend\n",
    );
    round_trip("export open class Base\nend\n");
    round_trip(
        "class M\n\tfunction __init(self)\n\tend\n\tfunction __tostring(self)\n\t\treturn \"m\"\n\tend\nend\n",
    );
}

/// `class` and `open` stay ordinary names outside a declaration.
#[test]
fn class_and_open_stay_contextual() {
    round_trip("local class = 1\nclass = class + 1\n");
    round_trip("local open = io.open\nopen(\"f\")\n");
    round_trip("class.method()\n");
    round_trip("return open\n");
}

#[test]
fn a_class_rejects_what_the_rfc_rejects() {
    rejects("class Point\n\tfunction __index(self)\n\tend\nend\n");
    rejects("class Point\n\tfunction a.b(self)\n\tend\nend\n");
    rejects("class Point\n\tpublic x: number\n");
}

#[test]
fn export_by_value_forms_parse() {
    round_trip("export local version = \"5.1\"\n");
    round_trip("export const TAU = math.pi * 2\n");
    round_trip("export local a, b, c = 1, 2, 3\n");
    round_trip("export function init()\nend\n");
    round_trip("@native\nexport function fast()\nend\n");
}

/// A variable named export keeps parsing as an expression.
#[test]
fn export_stays_contextual() {
    round_trip("local export = 1\nexport = export + 1\n");
    round_trip("export.field = 2\n");
}

#[test]
fn integer_literals_parse() {
    round_trip("local n = 123i\n");
    round_trip("local h = 0xABABi + 0xf_fi\n");
    round_trip("local b = 0b1000_1000i\n");
    round_trip("local big = 0xFFFF_FFFF_FFFF_FFFFi\n");
}
