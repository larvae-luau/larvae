/*!
darklua parity rules

Each rule here matches a darklua rule name and its documented behavior.
Thus a ported config does the same work as before. The implementations go
through the shared engine. They walk the tree, push byte edits, and keep
newline counts when they delete multiline spans.

A rule that cannot prove a transform safe from the tree alone skips that
instance without a report. A conservative result is better than a wrong
result. The rules never see the output of each other. For this reason,
each result that darklua reaches through a chain of rules happens here in
one pass.
*/

mod assign;
mod calls;
mod eval;
mod exprs;
mod flow;
mod fold;
mod interp;
mod locals;
mod methods;
mod support;
mod types;

use crate::config::RulesConfig;
use crate::diag::Diag;
use crate::rules::edits::Edits;
use crate::rules::engine::RuleCtx;
use std::path::Path;

/// True when one or more rules in this module are enabled. This gates the parse.
pub fn wants(cfg: &RulesConfig) -> bool {
    cfg.remove_method_definition
        || cfg.remove_compound_assignment
        || cfg.remove_floor_division
        || cfg.remove_if_expression
        || cfg.remove_method_call
        || cfg.convert_index_to_field
        || cfg.convert_function_to_assignment
        || cfg.convert_luau_number
        || cfg.make_assignment_local
        || cfg.remove_types
        || cfg.remove_function_call_parens
        || cfg.filter_after_early_return
        || cfg.remove_continue
        || cfg.compute_expression
        || cfg.remove_unused_if_branch
        || cfg.remove_unused_while
        || cfg.remove_empty_do
        || cfg.remove_nil_declaration
        || cfg.group_local_assignment
        || cfg.convert_local_function_to_assign
        || cfg.convert_square_root_call
        || cfg.remove_unused_variable
        || cfg.rename_variables
        || cfg.remove_attribute.as_ref().is_some_and(|r| r.enabled())
        || cfg
            .remove_interpolated_string
            .as_ref()
            .is_some_and(|r| r.enabled())
        || cfg.remove_assertions.as_ref().is_some_and(|r| r.enabled())
        || cfg
            .remove_debug_profiling
            .as_ref()
            .is_some_and(|r| r.enabled())
}

/// Run each enabled rule. Push edits and diagnostics.
pub fn apply(
    cfg: &RulesConfig,
    ctx: &RuleCtx,
    edits: &mut Edits,
    _diags: &mut Vec<Diag>,
    _path: &Path,
) {
    /*
    convert_function_to_assignment already rewrites a method definition
    head and inserts the self parameter. If remove_method_definition also
    added one, the output would hold `self, self`. For this reason, the
    broader rule wins.
    */
    if cfg.remove_method_definition && !cfg.convert_function_to_assignment {
        edits.run("remove_method_definition", |e| {
            methods::remove_method_definition(ctx, e)
        });
    }

    if cfg.convert_function_to_assignment {
        edits.run("convert_function_to_assignment", |e| {
            methods::convert_function_to_assignment(ctx, e)
        });
    }

    if cfg.convert_local_function_to_assign {
        edits.run("convert_local_function_to_assign", |e| {
            methods::convert_local_function_to_assign(ctx, e)
        });
    }

    if cfg.remove_method_call {
        edits.run("remove_method_call", |e| {
            methods::remove_method_call(ctx, e)
        });
    }

    if cfg.remove_compound_assignment {
        edits.run("remove_compound_assignment", |e| {
            assign::remove_compound_assignment(ctx, e, cfg.remove_floor_division)
        });
    }

    if cfg.remove_floor_division {
        edits.run("remove_floor_division", |e| {
            assign::remove_floor_division(ctx, e)
        });
    }

    if cfg.make_assignment_local {
        edits.run("make_assignment_local", |e| {
            assign::make_assignment_local(ctx, e)
        });
    }

    if cfg.remove_nil_declaration {
        edits.run("remove_nil_declaration", |e| {
            assign::remove_nil_declaration(ctx, e)
        });
    }

    if cfg.group_local_assignment {
        edits.run("group_local_assignment", |e| {
            assign::group_local_assignment(ctx, e)
        });
    }

    if cfg.remove_if_expression {
        edits.run("remove_if_expression", |e| {
            exprs::remove_if_expression(ctx, e)
        });
    }

    if cfg.convert_index_to_field {
        edits.run("convert_index_to_field", |e| {
            exprs::convert_index_to_field(ctx, e)
        });
    }

    if cfg.convert_luau_number {
        edits.run("convert_luau_number", |e| {
            exprs::convert_luau_number(ctx, e)
        });
    }

    if cfg.remove_function_call_parens {
        edits.run("remove_function_call_parens", |e| {
            exprs::remove_function_call_parens(ctx, e)
        });
    }

    if cfg.convert_square_root_call {
        edits.run("convert_square_root_call", |e| {
            exprs::convert_square_root_call(ctx, e)
        });
    }

    if cfg.remove_types {
        edits.run("remove_types", |e| types::remove_types(ctx, e));
    }

    if let Some(r) = &cfg.remove_attribute
        && r.enabled()
    {
        edits.run("remove_attribute", |e| {
            types::remove_attribute(ctx, e, r.patterns())
        });
    }

    if let Some(r) = &cfg.remove_interpolated_string
        && r.enabled()
    {
        edits.run("remove_interpolated_string", |e| {
            interp::remove_interpolated_string(ctx, e, r.strategy())
        });
    }

    if cfg.filter_after_early_return {
        edits.run("filter_after_early_return", |e| {
            flow::filter_after_early_return(ctx, e)
        });
    }

    if cfg.remove_continue {
        edits.run("remove_continue", |e| flow::remove_continue(ctx, e));
    }

    if cfg.remove_unused_while {
        edits.run("remove_unused_while", |e| flow::remove_unused_while(ctx, e));
    }

    if cfg.remove_unused_if_branch {
        edits.run("remove_unused_if_branch", |e| {
            flow::remove_unused_if_branch(ctx, e)
        });
    }

    if cfg.remove_empty_do {
        edits.run("remove_empty_do", |e| flow::remove_empty_do(ctx, e));
    }

    if let Some(r) = &cfg.remove_assertions
        && r.enabled()
    {
        edits.run("remove_assertions", |e| {
            calls::remove_assertions(ctx, e, r.preserve())
        });
    }

    if let Some(r) = &cfg.remove_debug_profiling
        && r.enabled()
    {
        edits.run("remove_debug_profiling", |e| {
            calls::remove_debug_profiling(ctx, e, r.preserve())
        });
    }

    if cfg.remove_unused_variable {
        edits.run("remove_unused_variable", |e| {
            locals::remove_unused_variable(ctx, e)
        });
    }

    if cfg.rename_variables {
        edits.run("rename_variables", |e| locals::rename_variables(ctx, e));
    }

    if cfg.compute_expression {
        edits.run("compute_expression", |e| fold::compute_expression(ctx, e));
    }
}

#[cfg(test)]
pub(crate) mod testing {
    /*
    Each rule test needs the same steps. Parse a snippet, run one rule,
    splice the edits in the same way as the pipeline, and compare the
    text.
    */
    use crate::rules::edits::{Edit, Edits, splice};
    use crate::rules::engine::RuleCtx;
    use crate::syntax::{lexer, parser};

    pub fn run(src: &str, rule: impl Fn(&RuleCtx, &mut Vec<Edit>)) -> String {
        let lexed = lexer::lex(src).expect("lexes");
        let chunk = parser::parse(src, &lexed.toks).expect("parses");

        let ctx = RuleCtx {
            src,
            toks: &lexed.toks,
            chunk: &chunk,
            comments: &lexed.comments,
            require_forms: &[],
            dm_path: None,
            quote: '"',
            defines: &Default::default(),
            globals: &Default::default(),
        };

        let mut edits = Edits::new();
        edits.run("rule under test", |e| rule(&ctx, e));

        splice(src, &edits, &mut Vec::new())
    }

    /// Each rule must keep the line count stable for retain-lines output.
    pub fn assert_lines_kept(before: &str, after: &str) {
        assert_eq!(
            before.bytes().filter(|&b| b == b'\n').count(),
            after.bytes().filter(|&b| b == b'\n').count(),
            "line count drifted\nbefore:\n{before}\nafter:\n{after}"
        );
    }
}
