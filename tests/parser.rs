/*!
Parser conformance, every snippet must parse, tile the token stream with no
holes, and print back byte for byte, that trio is the M1a exit criterion
*/

use larvae::syntax::{lexer, parser, printer};

/// Parse, check coverage, print, compare, all in one
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
    // a stack overflow here is the darklua bug class we designed out
    let deep = format!("local x = {}1{}", "(".repeat(5000), ")".repeat(5000));
    let lexed = lexer::lex(&deep).unwrap();

    assert!(parser::parse(&deep, &lexed.toks).is_err());

    let deep_tables = format!("local t = {}{}", "{".repeat(5000), "}".repeat(5000));
    let lexed = lexer::lex(&deep_tables).unwrap();

    assert!(parser::parse(&deep_tables, &lexed.toks).is_err());
}

/*
Poor man's fuzzing, byte level mutations of the corpus must never panic or
hang, real coverage guided fuzzing lives in fuzz/ for nightly runs
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
                    // must terminate and must not panic, either result is fine
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
