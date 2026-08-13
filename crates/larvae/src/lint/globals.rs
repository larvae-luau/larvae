/*!
The names that exist before a file runs.

`undefined_variable` is only as good as this list. A list with a missing name
turns a working file into many false reports. Thus the rule here is to
include a doubtful name, not to omit it. A global that exists but is not
listed causes a wrong warning. A listed global that does not exist costs a
warning that no user would get.

selene keeps these lists in TOML files that a project can extend or replace.
larvae ships the two lists that cover almost all users as static slices. The
reason is speed: a lookup that must parse a file cannot run on every
keystroke. A project with its own globals lists them under `[lint] globals`
and does not write a library file.
*/

use super::config::StdLib;

/// Luau's own globals. Every Luau host provides them.
pub const LUAU: &[&str] = &[
    "_G",
    "_VERSION",
    "assert",
    "bit32",
    "buffer",
    "collectgarbage",
    "coroutine",
    "debug",
    "error",
    "getfenv",
    "getmetatable",
    "ipairs",
    "loadstring",
    "math",
    "newproxy",
    "next",
    "os",
    "pairs",
    "pcall",
    "print",
    "rawequal",
    "rawget",
    "rawlen",
    "rawset",
    "require",
    "select",
    "setfenv",
    "setmetatable",
    "string",
    "table",
    "tonumber",
    "tostring",
    "type",
    "typeof",
    "unpack",
    "utf8",
    "vector",
    "warn",
    "xpcall",
];

/// The names that Roblox adds: the data types and the globals that its scripts get.
pub const ROBLOX: &[&str] = &[
    // The entry points.
    "game",
    "plugin",
    "script",
    "shared",
    "workspace",
    // Scheduling. The list includes the deprecated forms, so larvae does not
    // report them as undefined.
    "DebuggerManager",
    "PluginManager",
    "UserSettings",
    "delay",
    "elapsedTime",
    "gcinfo",
    "printidentity",
    "settings",
    "spawn",
    "stats",
    "task",
    "tick",
    "time",
    "version",
    "wait",
    // The data types.
    "Axes",
    "BrickColor",
    "CFrame",
    "CatalogSearchParams",
    "Color3",
    "ColorSequence",
    "ColorSequenceKeypoint",
    "Content",
    "DateTime",
    "DockWidgetPluginGuiInfo",
    "Enum",
    "Faces",
    "FloatCurveKey",
    "Font",
    "Instance",
    "NumberRange",
    "NumberSequence",
    "NumberSequenceKeypoint",
    "OverlapParams",
    "PathWaypoint",
    "PhysicalProperties",
    "Random",
    "Ray",
    "RaycastParams",
    "Rect",
    "Region3",
    "Region3int16",
    "RotationCurveKey",
    "SecurityCapabilities",
    "SharedTable",
    "TweenInfo",
    "UDim",
    "UDim2",
    "Vector2",
    "Vector2int16",
    "Vector3",
    "Vector3int16",
];

/// Returns true if this name exists before the file runs.
pub fn has(std: StdLib, name: &str) -> bool {
    match std {
        StdLib::None => false,

        StdLib::Luau => LUAU.binary_search(&name).is_ok(),

        StdLib::Roblox => LUAU.binary_search(&name).is_ok() || ROBLOX.contains(&name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `has` does a binary search in Luau's list, so the list must stay sorted.
    #[test]
    fn the_luau_list_is_sorted() {
        let mut sorted = LUAU.to_vec();
        sorted.sort_unstable();

        assert_eq!(LUAU, sorted.as_slice());
    }

    #[test]
    fn no_name_is_listed_twice() {
        for list in [LUAU, ROBLOX] {
            let mut seen = list.to_vec();
            seen.sort_unstable();
            let before = seen.len();
            seen.dedup();

            assert_eq!(seen.len(), before);
        }
    }

    /// Roblox adds to Luau and does not replace it.
    #[test]
    fn the_two_lists_do_not_overlap() {
        for name in ROBLOX {
            assert!(!LUAU.contains(name), "{name} is in both lists");
        }
    }

    #[test]
    fn luau_globals_are_found_under_both_libraries() {
        for name in ["print", "pairs", "table", "typeof"] {
            assert!(has(StdLib::Luau, name), "{name}");
            assert!(has(StdLib::Roblox, name), "{name}");
        }
    }

    #[test]
    fn roblox_globals_are_found_only_under_roblox() {
        for name in ["game", "Instance", "task", "Vector3"] {
            assert!(has(StdLib::Roblox, name), "{name}");
            assert!(!has(StdLib::Luau, name), "{name}");
        }
    }

    #[test]
    fn nothing_exists_under_none() {
        assert!(!has(StdLib::None, "print"));
        assert!(!has(StdLib::None, "game"));
    }

    #[test]
    fn a_name_nobody_defines_is_not_found() {
        assert!(!has(StdLib::Roblox, "definitelyNotAGlobal"));
    }

    /*
    The deprecated scheduling globals still exist at runtime. To omit them
    would report working code as undefined. A different lint reports that
    they are deprecated.
    */
    #[test]
    fn deprecated_globals_are_still_globals() {
        for name in ["wait", "spawn", "delay", "tick"] {
            assert!(has(StdLib::Roblox, name), "{name}");
        }
    }
}
