/*!
Every lint, grouped by the kind of mistake that it catches.

The grouping helps the reader and does not change behavior. A user configures
a lint by its name, and the name stays the same in every file. Thus to move a
lint between these files changes nothing that a project can see.
*/

pub mod conditionals;
pub mod configured;
pub mod correctness;
pub mod luau;
pub mod names;
pub mod original;
pub mod roblox;
pub mod shape;
pub mod style;

use super::Lint;

/*
The registry.

The order decides only which finding larvae lists first when two findings
land on the same byte. Thus the order follows the severity of the
consequence: a wrong thing comes before an untidy thing.
*/
pub static ALL: &[&dyn Lint] = &[
    // Correctness.
    &correctness::AlmostSwapped,
    &correctness::CompareNan,
    &correctness::ConstantTableComparison,
    &correctness::DivideByZero,
    &correctness::DuplicateKeys,
    &correctness::IfsSameCond,
    &correctness::IfSameThenElse,
    &correctness::SuspiciousReverseLoop,
    &correctness::TypeCheckInsideCall,
    &correctness::UnbalancedAssignments,
    &luau::ZeroStepLoop,
    &configured::BadStringEscape,
    &configured::MismatchedArgCount,
    &configured::MustUse,
    // The lints that Luau's own compiler has.
    &luau::BadCommentDirective,
    &luau::BuiltinGlobalWrite,
    &luau::ComparisonPrecedence,
    &luau::DuplicateFunction,
    &luau::DuplicateLocal,
    &luau::FormatString,
    &luau::ImplicitReturn,
    &luau::MisleadingAndOr,
    &luau::NumberLiteralOverflow,
    &luau::PlaceholderRead,
    &luau::TableOperations,
    &luau::ImplicitAnyLocal,
    &luau::ImplicitAnyParameter,
    &luau::UninitializedLocal,
    &luau::UnknownType,
    // Names.
    &names::UndefinedVariable,
    &names::UnscopedVariables,
    &names::UnusedFunction,
    &names::UnusedVariable,
    &names::Shadowing,
    &names::GlobalUsage,
    // The lints that a project tunes.
    &configured::Deprecated,
    &configured::RestrictedModulePaths,
    &configured::HighCyclomaticComplexity,
    &configured::ManualTableClone,
    &configured::PreferConst,
    &configured::RestrictedGlobals,
    // Roblox data types.
    &roblox::RobloxIncorrectColor3NewBounds,
    &roblox::RobloxSuspiciousUdim2New,
    &roblox::RobloxManualFromScaleOrOffset,
    // The lints that selene does not have.
    &original::NonConstRequire,
    &original::UnreachableCode,
    &original::SelfAssignment,
    &original::StringConcatInLoop,
    &original::ShadowedLoopWork,
    &original::LengthAsCondition,
    &original::BuiltinShadowed,
    &original::IgnoredPcallResult,
    // Style.
    &style::EmptyIf,
    &style::EmptyLoop,
    &style::MixedTable,
    &style::MultipleStatements,
    &style::ParentheseConditions,
    &shape::ConstantCondition,
    &shape::ElseAfterReturn,
    &shape::CollapsibleIf,
    &shape::NegatedCondition,
    &conditionals::AndOrConditional,
    &conditionals::IfExpressionAssignment,
];

/*
The boilerplate that every lint shares.

Each entry declares the unit type, the name that a user writes, the group it
belongs to, the default level, and the one-line explanation. The lint itself
is the `check` function, which the author writes as normal code. Thus the
author writes only the part that differs per lint.

The group is what a project sets under `[lint.groups]`, and it is also how
`--explain` and the docs order the list. The file a lint lives in is a
separate thing and stays that way: these files split by where a lint came
from, so `configured.rs` holds a compile error beside an opinion about branch
counts. That is a useful editing category and a useless configuration one.
*/
#[macro_export]
macro_rules! lints {
    ($($ty:ident => $name:literal, $group:ident, $level:ident, $about:literal;)*) => {
        $(
            pub struct $ty;

            impl $crate::lint::Lint for $ty {
                fn name(&self) -> &'static str {
                    $name
                }

                fn default_level(&self) -> $crate::lint::Level {
                    $crate::lint::Level::$level
                }

                fn group(&self) -> $crate::lint::Group {
                    $crate::lint::Group::$group
                }

                fn about(&self) -> &'static str {
                    $about
                }

                fn run(
                    &self,
                    ctx: &$crate::lint::LintCtx<'_>,
                    out: &mut Vec<$crate::lint::Finding>,
                ) {
                    $ty::check(ctx, out)
                }
            }
        )*
    };
}
