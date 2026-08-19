//! The tests for the Luau form, kept out of the code they exercise.

use super::*;

mod core_tests {
    use super::*;

    fn worm(body: &str) -> LuauWorm {
        LuauWorm::load(body, "test").expect("worm loads")
    }

    const ECHO: &str = r#"
return {
    frontend = {
        compile = function(source, config)
            return source .. "|" .. config
        end,
    },
}
"#;

    #[test]
    fn a_string_survives_the_round_trip() {
        let out = worm(ECHO).transform("hello", "cfg").unwrap();

        assert!(out.ok);
        assert_eq!(out.text, "hello|cfg");
    }

    #[test]
    fn utf8_and_newlines_cross_intact() {
        let src = "local s = \"héllo ✓\"\nreturn s\n";
        let out = worm(ECHO).transform(src, "").unwrap();

        assert_eq!(out.text, format!("{src}|"));
    }

    #[test]
    fn one_instance_serves_many_files() {
        let mut w = worm(ECHO);

        for i in 0..500 {
            let src = format!("file {i}");

            assert_eq!(w.transform(&src, "c").unwrap().text, format!("{src}|c"));
        }
    }

    #[test]
    fn a_worm_reporting_a_problem_is_not_an_error() {
        let out = worm(
            r#"
return { frontend = { compile = function() error("refused, bad tag") end } }
"#,
        )
        .transform("x", "")
        .unwrap();

        assert!(!out.ok);
        assert!(out.text.contains("refused, bad tag"), "{}", out.text);
    }

    #[test]
    fn a_module_without_a_frontend_is_refused_at_load() {
        let err = LuauWorm::load("return {}", "test").err().unwrap();

        assert!(err.to_string().contains("frontend"), "{err}");
    }

    #[test]
    fn syntax_errors_are_refused_at_load() {
        assert!(LuauWorm::load("this is not luau ===", "test").is_err());
    }

    #[test]
    fn returning_the_wrong_type_is_an_error() {
        let err = worm("return { frontend = { compile = function() return 42 end } }")
            .transform("x", "")
            .unwrap_err();

        assert!(err.to_string().contains("expected a string"), "{err}");
    }

    /// One bad file must not end a watch session
    #[test]
    fn a_failure_does_not_poison_the_next_call() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source)
            if source == "BAD" then error("no") end
            return source
        end,
    },
}
"#,
        );

        assert!(!w.transform("BAD", "").unwrap().ok);
        assert_eq!(w.transform("good", "").unwrap().text, "good");
    }

    /// The sandbox is the reason a worm cannot reach the filesystem
    #[test]
    fn the_sandbox_denies_ambient_authority() {
        let out = worm(
            r#"
return {
    frontend = {
        compile = function()
            if io ~= nil or os ~= nil and os.execute ~= nil then
                return "REACHABLE"
            end
            return "sandboxed"
        end,
    },
}
"#,
        )
        .transform("x", "")
        .unwrap();

        assert_eq!(out.text, "sandboxed");
    }

    #[test]
    fn an_endless_worm_is_stopped_rather_than_hanging() {
        let out = worm(
            r#"
return { frontend = { compile = function() while true do end end } }
"#,
        )
        .transform("x", "")
        .unwrap();

        assert!(!out.ok);
        assert!(out.text.contains("too long"), "{}", out.text);
    }
}

mod rule_tests {
    use super::*;
    use crate::syntax::{lexer, parser};
    use crate::worm::nodes::{Kind, NodeTable};

    fn file(src: &str) -> Arc<FileCtx> {
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

        Arc::new(FileCtx::new(
            NodeTable::build(&chunk, &bytes),
            src.to_owned(),
            "test.luau".into(),
        ))
    }

    fn matching(f: &FileCtx, kind: Kind) -> Vec<u32> {
        f.table
            .iter()
            .filter(|(_, n)| n.kind == kind)
            .map(|(id, _)| id)
            .collect()
    }

    const STRIP: &str = r#"
return {
    rules = {
        strip = {
            visit = function(node, ctx)
                if node:kind() == "Number" then
                    ctx:replace(node, "0")
                end
            end,
        },
    },
}
"#;

    #[test]
    fn a_rule_reads_nodes_and_queues_edits() {
        let mut w = LuauWorm::load(STRIP, "t").unwrap();
        let f = file("local x = 41\n");
        let ids = matching(&f, Kind::Number);

        let edits = w.run_rule("strip", Arc::clone(&f), &ids).unwrap();

        assert_eq!(edits, [(10, 12, "0".to_owned())]);
    }

    #[test]
    fn a_worm_can_ship_rules_without_a_frontend() {
        let w = LuauWorm::load(STRIP, "t").unwrap();

        assert_eq!(w.rule_names().collect::<Vec<_>>(), ["strip"]);
    }

    #[test]
    fn a_worm_with_neither_role_is_refused() {
        let err = LuauWorm::load("return { rules = {} }", "t").err().unwrap();

        assert!(err.to_string().contains("neither"), "{err}");
    }

    #[test]
    fn text_and_span_and_children_all_reach_the_real_tree() {
        let src = "print(\"a\", 2)\n";
        let mut w = LuauWorm::load(
            r#"
return {
    rules = {
        probe = {
            visit = function(node, ctx)
                local kids = node:children()
                local start, stop = node:span()
                ctx:replace(node, node:text() .. "|" .. #kids .. "|" .. start .. ":" .. stop)
            end,
        },
    },
}
"#,
            "t",
        )
        .unwrap();

        let f = file(src);
        let ids = matching(&f, Kind::CallExpr);
        let edits = w.run_rule("probe", Arc::clone(&f), &ids).unwrap();

        assert_eq!(edits[0].2, "print(\"a\", 2)|3|0:13");
    }

    #[test]
    fn parent_walks_back_up() {
        let mut w = LuauWorm::load(
            r#"
return {
    rules = {
        up = {
            visit = function(node, ctx)
                ctx:replace(node, node:parent():kind())
            end,
        },
    },
}
"#,
            "t",
        )
        .unwrap();

        let f = file("local x = 1\n");
        let ids = matching(&f, Kind::Number);

        assert_eq!(
            w.run_rule("up", Arc::clone(&f), &ids).unwrap()[0].2,
            "Local"
        );
    }

    /// This test shows the reason a handle carries an epoch
    #[test]
    fn a_handle_stashed_across_files_is_caught() {
        let mut w = LuauWorm::load(
            r#"
local stashed = nil
return {
    rules = {
        stash = {
            visit = function(node, ctx)
                if stashed == nil then
                    stashed = node
                else
                    ctx:replace(stashed, "boom")
                end
            end,
        },
    },
}
"#,
            "t",
        )
        .unwrap();

        let first = file("local a = 1\n");
        let ids = matching(&first, Kind::Number);
        w.run_rule("stash", Arc::clone(&first), &ids).unwrap();

        // on the second file, the worm uses the handle it kept
        let second = file("local b = 2\n");
        let ids = matching(&second, Kind::Number);
        let err = w.run_rule("stash", Arc::clone(&second), &ids).unwrap_err();

        assert!(err.to_string().contains("outlived its file"), "{err}");
    }

    #[test]
    fn remove_preserves_the_line_count() {
        let src = "local f = function()\n  return 1\nend\n";
        let mut w = LuauWorm::load(
            r#"
return { rules = { drop = { visit = function(node, ctx) ctx:remove(node) end } } }
"#,
            "t",
        )
        .unwrap();

        let f = file(src);
        let ids = matching(&f, Kind::FunctionExpr);
        let edits = w.run_rule("drop", Arc::clone(&f), &ids).unwrap();

        let (start, end, text) = &edits[0];
        assert_eq!(
            src[*start as usize..*end as usize].matches('\n').count(),
            text.matches('\n').count()
        );
    }

    #[test]
    fn init_hands_over_config_and_only_the_enabled_rules() {
        let mut w = LuauWorm::load(
            r#"
local seen = nil
return {
    init = function(config, rules)
        seen = config.factory .. "/" .. tostring(rules.strip)
    end,
    rules = { strip = { visit = function(node, ctx) ctx:replace(node, seen) end } },
}
"#,
            "t",
        )
        .unwrap();

        let config = toml::from_str::<toml::Value>("factory = \"vide\"").unwrap();
        let rules = BTreeMap::from([("strip".to_owned(), toml::Value::Boolean(true))]);

        // the default settings also show that a two-argument init keeps working
        w.init(&config, &rules, &Default::default()).unwrap();

        let f = file("local x = 1\n");
        let ids = matching(&f, Kind::Number);

        assert_eq!(
            w.run_rule("strip", Arc::clone(&f), &ids).unwrap()[0].2,
            "vide/true"
        );
    }
}

mod frontend_tests {
    use super::*;
    use crate::fmt::FmtConfig;

    fn worm(body: &str) -> LuauWorm {
        LuauWorm::load(body, "test").expect("worm loads")
    }

    /// The full path: a worm document with a host span, rendered by the host
    #[test]
    fn a_document_with_a_host_span_renders_in_the_project_style() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        format = function(source)
            local at = string.find(source, "local", 1, true) - 1
            return {
                document = {
                    concat = {
                        { src = { 0, at } },
                        { host = { start = at, ["end"] = #source, parse = "block" } },
                    },
                },
                comments = {},
            }
        end,
    },
}
"#,
        );

        let src = "<Frame>\nlocal  x  =  1";
        let reply = w.format(src).unwrap();
        let out = proto::render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert_eq!(out, "<Frame>\nlocal x = 1\n");
    }

    /// The least a worm can do: name the Luau, and larvae lays it out
    #[test]
    fn named_luau_spans_format_through_the_host() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        format = function(source)
            local first = string.find(source, "local", 1, true) - 1
            local stop = string.find(source, "\n</Frame>", 1, true) - 1
            return { spans = { { first, stop } } }
        end,
    },
}
"#,
        );

        let src = "<Frame>\nlocal  x   =  1\n</Frame>\n";
        let reply = w.format(src).unwrap();
        let out = proto::render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert_eq!(out, "<Frame>\nlocal x = 1\n</Frame>\n");
    }

    #[test]
    fn a_lint_reply_crosses_with_findings_and_a_shadow() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        lint = function(source)
            return {
                findings = {
                    { span = { 0, 4 }, lint = "tidy", message = "untidy", help = "tidy it" },
                },
                comments = { { 5, 9 } },
                luau = string.rep(" ", #source),
            }
        end,
    },
}
"#,
        );

        let reply = w.lint("ab cd efgh").unwrap();

        assert_eq!(reply.findings.len(), 1);
        assert_eq!(reply.findings[0].span, (0, 4));
        assert_eq!(reply.findings[0].lint, "tidy");
        assert_eq!(reply.findings[0].message, "untidy");
        assert_eq!(reply.findings[0].help.as_deref(), Some("tidy it"));
        assert_eq!(reply.comments, vec![(5, 9)]);
        assert_eq!(reply.luau.as_deref(), Some("          "));
    }

    /// An empty list and an absent list both mean "none"
    #[test]
    fn empty_and_absent_lists_both_cross() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        lint = function() return { findings = {} } end,
    },
}
"#,
        );

        let reply = w.lint("x").unwrap();

        assert!(reply.findings.is_empty());
        assert!(reply.comments.is_empty());
        assert_eq!(reply.luau, None);
    }

    /// The manifest promised a capability that the table does not supply
    #[test]
    fn a_missing_format_function_names_the_manifest_flag() {
        let mut w = worm("return { frontend = { compile = function(source) return source end } }");

        let err = w.format("x").unwrap_err();

        assert!(
            err.to_string()
                .contains("sets fmt = true but its table has no frontend.format"),
            "{err}"
        );
    }

    #[test]
    fn a_missing_lint_function_names_the_manifest_declaration() {
        let mut w = worm("return { frontend = { compile = function(source) return source end } }");

        let err = w.lint("x").unwrap_err();

        assert!(
            err.to_string()
                .contains("declares lints but its table has no frontend.lint"),
            "{err}"
        );
    }

    /// The settings arrive as tables, so no worm parses JSON
    #[test]
    fn init_hands_over_the_settings_as_tables() {
        let mut w = worm(
            r#"
local seen = nil
return {
    init = function(config, rules, settings)
        seen = tostring(settings.fmt.column_width) .. "/" .. tostring(settings.lint.tidy)
    end,
    frontend = {
        compile = function(source) return source end,
        lint = function()
            return {
                findings = {
                    { span = { 0, 1 }, lint = "probe", message = "saw " .. seen },
                },
            }
        end,
    },
}
"#,
        );

        let settings = crate::worm::Settings {
            fmt: r#"{"column_width":88}"#.to_owned(),
            lint: r#"{"tidy":"warn"}"#.to_owned(),
        };
        let config = toml::from_str::<toml::Value>("").unwrap();

        w.init(&config, &BTreeMap::new(), &settings).unwrap();

        let reply = w.lint("x").unwrap();

        assert_eq!(reply.findings[0].message, "saw 88/warn");
    }

    /// The version field is an in-tree contract, so the host fills it
    #[test]
    fn a_reply_without_a_doc_field_gets_the_host_version() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        format = function() return { document = "nil" } end,
    },
}
"#,
        );

        let reply = w.format("x").unwrap();

        assert_eq!(reply.doc, proto::DOC_VERSION);
        assert_eq!(reply.document, Some(proto::WireDoc::Nil));
    }

    /// A worm error carries the reason the worm wrote, not a traceback
    #[test]
    fn a_format_error_carries_the_worm_message() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        format = function() error("line 2 is not markup") end,
    },
}
"#,
        );

        let err = w.format("x").unwrap_err();

        assert!(err.to_string().contains("line 2 is not markup"), "{err}");
    }

    /// A reply of the wrong shape is refused with the reason, not a panic
    #[test]
    fn a_malformed_reply_is_refused() {
        let mut w = worm(
            r#"
return {
    frontend = {
        compile = function(source) return source end,
        format = function() return "not a table" end,
    },
}
"#,
        );

        let err = w.format("x").unwrap_err();

        assert!(
            format!("{err:#}").contains("does not have the documented shape"),
            "{err:#}"
        );
    }
}

mod editor_paths {
    use super::*;

    fn worm(body: &str) -> LuauWorm {
        LuauWorm::load(body, "t").expect("the worm loads")
    }

    const FRONTEND: &str = r#"
return {
    frontend = {
        compile = function(source) return { output = source } end,
        actions = function(source, start, stop)
            return { actions = { {
                title = "Trim",
                edits = { { span = { start, stop }, text = "x" } },
                fixes = "bad",
            } } }
        end,
        definitions = function()
            return { definitions = "declare thing: number\n" }
        end,
    },
}
"#;

    #[test]
    fn a_luau_worm_offers_actions() {
        let reply = worm(FRONTEND).actions("hello", (1, 4)).unwrap();

        assert_eq!(reply.actions.len(), 1);
        assert_eq!(reply.actions[0].title, "Trim");
        assert_eq!(reply.actions[0].edits[0].span, (1, 4));
        assert_eq!(reply.actions[0].fixes.as_deref(), Some("bad"));
    }

    #[test]
    fn a_luau_worm_supplies_definitions() {
        let reply = worm(FRONTEND).definitions().unwrap();

        assert!(reply.definitions.contains("declare thing"), "{reply:?}");
    }

    /*
    A worm without the functions answers with nothing.

    The editor asks on a keystroke, so a worm that only compiles must cost a
    reply and not an error.
    */
    #[test]
    fn a_luau_worm_without_them_is_quiet() {
        const BARE: &str = r#"
return { frontend = { compile = function(s) return { output = s } end } }
"#;

        assert!(worm(BARE).actions("x", (0, 1)).unwrap().actions.is_empty());
        assert!(worm(BARE).definitions().unwrap().definitions.is_empty());
    }
}
