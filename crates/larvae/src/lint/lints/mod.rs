/*!
Every lint, grouped by the kind of mistake that it catches.

The grouping helps the reader and does not change behavior. A user configures
a lint by its name, and the name stays the same in every file. Thus to move a
lint between these files changes nothing that a project can see.
*/

pub mod configured;
pub mod correctness;
pub mod luau;
pub mod names;
pub mod original;
pub mod roblox;
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
    &luau::UninitializedLocal,
    &luau::UnknownType,
    // Names.
    &names::UndefinedVariable,
    &names::UnscopedVariables,
    &names::UnusedVariable,
    &names::Shadowing,
    &names::GlobalUsage,
    // The lints that a project tunes.
    &configured::Deprecated,
    &configured::RestrictedModulePaths,
    &configured::HighCyclomaticComplexity,
    &configured::ManualTableClone,
    &configured::PreferConst,
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
    // Style.
    &style::EmptyIf,
    &style::EmptyLoop,
    &style::MixedTable,
    &style::MultipleStatements,
    &style::ParentheseConditions,
];

/*
The boilerplate that every lint shares.

Each entry declares the unit type, the name that a user writes, the default
level, and the one-line explanation. The lint itself is the `check` function,
which the author writes as normal code. Thus the author writes only the part
that differs per lint.
*/
#[macro_export]
macro_rules! lints {
    ($($ty:ident => $name:literal, $level:ident, $about:literal;)*) => {
        $(
            pub struct $ty;

            impl $crate::lint::Lint for $ty {
                fn name(&self) -> &'static str {
                    $name
                }

                fn default_level(&self) -> $crate::lint::Level {
                    $crate::lint::Level::$level
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
