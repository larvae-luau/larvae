use std::path::Path;

use larvae::config::lsp::FFlagsConfig;
use larvae::lsp::analysis::Analysis;

use crate::analyzer::{LuauAnalysis, apply_flags, larvae_reset_flags, luau_globals};

struct Flags(#[allow(dead_code)] luau_globals::Exclusive);

impl Drop for Flags {
    fn drop(&mut self) {
        unsafe { larvae_reset_flags() };
    }
}

fn enum_complaints() -> Vec<String> {
    let mut analysis = LuauAnalysis::new();
    let path = Path::new("/enums.luau");

    analysis.open(
        path,
        "--!strict\n\
         local key: Enum.KeyCode = Enum.KeyCode.A\n\
         local direction: Enum.EasingDirection = Enum.EasingDirection.In\n\
         local state: Enum.UserInputState = Enum.UserInputState.Begin\n\
         local list: { Enum.KeyCode } = {}\n\
         local by: { [Enum.KeyCode]: Enum.UserInputState } = {}\n\
         local maybe: Enum.Material? = nil\n\
         local function on(s: Enum.UserInputState): Enum.KeyCode\n\
         \treturn Enum.KeyCode.A\n\
         end\n\
         -- The global is the class the dump declares, beside its table of enums.\n\
         local every: Enums = Enum\n\
         return key, direction, state, list, by, maybe, on, every\n",
    );

    analysis
        .check(path)
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn roblox_enums_are_qualified_types_under_both_solvers() {
    let _flags = Flags(luau_globals::exclusive());

    let old_solver = enum_complaints();
    assert!(old_solver.is_empty(), "old solver: {old_solver:?}");

    let mut flags = FFlagsConfig::default();
    flags.enable_new_solver = true;
    apply_flags(&flags);

    let new_solver = enum_complaints();
    assert!(new_solver.is_empty(), "new solver: {new_solver:?}");
}
