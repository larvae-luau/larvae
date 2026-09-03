/*
The handlers roblox.cpp puts on the Roblox classes, under both solvers.

Each case is a script luau-lsp reads without complaint, or with one
complaint it names, and the analyzer here has to read it the same way.
The vector case is the one place this goes past luau-lsp on purpose.
*/

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

/// The complaints of one strict script under the default definitions
fn complaints(src: &str) -> Vec<String> {
    let mut analysis = LuauAnalysis::new();
    let path = Path::new("/magic.luau");

    analysis.open(path, src);

    analysis
        .check(path)
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

/// Run one check under the old solver, then under the new one
fn under_both(check: impl Fn(&str)) {
    let _flags = Flags(luau_globals::exclusive());

    check("old solver");

    apply_flags(&FFlagsConfig {
        enable_new_solver: true,
        ..Default::default()
    });

    check("new solver");
}

#[test]
fn is_a_refines_a_loop_variable_past_a_continue() {
    under_both(|solver| {
        let found = complaints(
            "--!strict\n\
             local areas = {}\n\
             local markers = workspace:WaitForChild(\"Markers\")\n\
             for _, instance in markers:GetChildren() do\n\
             \tif not instance:IsA(\"BasePart\") then\n\
             \t\tcontinue\n\
             \tend\n\
             \ttable.insert(areas, { name = instance.Name, position = instance.CFrame.Position })\n\
             end\n\
             for _, instance in markers:GetChildren() do\n\
             \tif instance:IsA(\"BasePart\") then\n\
             \t\tprint(instance.CFrame)\n\
             \tend\n\
             end\n\
             return areas\n",
        );

        assert!(found.is_empty(), "{solver}: {found:?}");
    });
}

#[test]
fn an_unknown_class_in_is_a_is_an_error() {
    under_both(|solver| {
        let found = complaints("--!strict\nprint(workspace:IsA(\"NotAClass\"))\n");

        assert!(
            found.iter().any(|m| m.contains("NotAClass")),
            "{solver}: {found:?}"
        );
    });
}

#[test]
fn vector3_and_vector_are_one_type() {
    under_both(|solver| {
        let src = "--!strict\n\
             local v = Vector3.new(1, 2, 3)\n\
             local list: { vector } = { v }\n\
             local function takes(p: vector) end\n\
             takes(v)\n\
             takes(workspace.CurrentCamera.CFrame.Position)\n\
             local m = vector.magnitude(v)\n\
             local made = vector.create(1, 2, 3)\n\
             local back: Vector3 = made\n\
             local frame = CFrame.new(made)\n\
             print(m, list, back, frame, made.Magnitude, made:Lerp(v, 0.5), (v + made).Unit)\n";

        let found = complaints(src);

        assert!(found.is_empty(), "{solver}: {found:?}");

        let mut analysis = LuauAnalysis::new();
        let path = Path::new("/vector.luau");

        analysis.open(path, src);

        let at = src.find("local made").expect("the binding") as u32 + 6;
        let card = analysis.hover(path, at, false, false).unwrap_or_default();

        assert!(card.contains("Vector3"), "{solver}: {card}");
    });
}

#[test]
fn clone_keeps_the_class() {
    under_both(|solver| {
        let found = complaints(
            "--!strict\n\
             local part = Instance.new(\"Part\")\n\
             local copy = part:Clone()\n\
             local again = Instance.fromExisting(part)\n\
             print(copy.CFrame, again.CFrame)\n",
        );

        assert!(found.is_empty(), "{solver}: {found:?}");
    });
}

#[test]
fn a_lookup_by_class_answers_with_the_class_or_nil() {
    under_both(|solver| {
        let found = complaints(
            "--!strict\n\
             local part = workspace:FindFirstChildWhichIsA(\"Part\")\n\
             if part then\n\
             \tprint(part.CFrame)\n\
             end\n\
             local model = workspace:FindFirstAncestorOfClass(\"Model\")\n\
             if model then\n\
             \tprint(model.PrimaryPart)\n\
             end\n",
        );

        assert!(found.is_empty(), "{solver}: {found:?}");

        let bare = complaints(
            "--!strict\n\
             local part = workspace:FindFirstChildWhichIsA(\"Part\")\n\
             print(part.CFrame)\n",
        );

        assert!(!bare.is_empty(), "{solver}: the answer is optional");
    });
}

#[test]
fn a_property_name_the_class_lacks_is_an_error() {
    under_both(|solver| {
        let found = complaints(
            "--!strict\n\
             local part = Instance.new(\"Part\")\n\
             part:GetPropertyChangedSignal(\"Nope\")\n",
        );

        assert!(
            found.iter().any(|m| m.contains("Nope")),
            "{solver}: {found:?}"
        );

        let fine = complaints(
            "--!strict\n\
             local part = Instance.new(\"Part\")\n\
             part:GetPropertyChangedSignal(\"CFrame\"):Connect(print)\n",
        );

        assert!(fine.is_empty(), "{solver}: {fine:?}");
    });
}

#[test]
fn a_service_or_class_outside_the_list_is_an_error() {
    under_both(|solver| {
        let found = complaints(
            "--!strict\n\
             local nope = game:GetService(\"NotAService\")\n\
             local none = Instance.new(\"NotAClass\")\n\
             print(nope, none)\n",
        );

        assert!(
            found
                .iter()
                .any(|m| m == "Invalid service name 'NotAService'"),
            "{solver}: {found:?}"
        );
        assert!(
            found.iter().any(|m| m == "Invalid class name 'NotAClass'"),
            "{solver}: {found:?}"
        );

        let fine = complaints(
            "--!strict\n\
             local players = game:GetService(\"Players\")\n\
             local part = Instance.new(\"Part\")\n\
             print(players.LocalPlayer, part.CFrame)\n",
        );

        assert!(fine.is_empty(), "{solver}: {fine:?}");
    });
}

#[test]
fn enum_item_is_a_refines() {
    under_both(|solver| {
        let found = complaints(
            "--!strict\n\
             local item: Enum.KeyCode | Enum.Material = Enum.KeyCode.A\n\
             if item:IsA(\"KeyCode\") then\n\
             \tlocal key: Enum.KeyCode = item\n\
             \tprint(key)\n\
             end\n",
        );

        assert!(found.is_empty(), "{solver}: {found:?}");
    });
}

#[test]
fn query_descendants_narrows_to_the_selector() {
    under_both(|solver| {
        let found = complaints(
            "--!strict\n\
             for _, part in workspace:QueryDescendants(\"Part\") do\n\
             \tprint(part.CFrame)\n\
             end\n\
             for _, either in workspace:QueryDescendants(\"Part, Model > Folder\") do\n\
             \tprint(either.Name)\n\
             end\n",
        );

        assert!(found.is_empty(), "{solver}: {found:?}");
    });
}

/// A small tree, the way the sourcemap declares one
const TREE: &str = "declare extern type _larvae_sourcemap_9_0 extends DataModel with\n\
     \tWorkspace: _larvae_sourcemap_9_1\n\
     end\n\
     declare extern type _larvae_sourcemap_9_1 extends Workspace with\n\
     \tParent: _larvae_sourcemap_9_0\n\
     \tMarkers: _larvae_sourcemap_9_2\n\
     end\n\
     declare extern type _larvae_sourcemap_9_2 extends Folder with\n\
     \tParent: _larvae_sourcemap_9_1\n\
     \tDeep: _larvae_sourcemap_9_3\n\
     end\n\
     declare extern type _larvae_sourcemap_9_3 extends Part with\n\
     \tParent: _larvae_sourcemap_9_2\n\
     end\n\
     declare game: _larvae_sourcemap_9_0\n";

fn tree_complaints(src: &str) -> Vec<String> {
    let mut analysis = LuauAnalysis::new();
    let path = Path::new("/tree.luau");

    assert!(analysis.definitions("@sourcemap", TREE));

    analysis.open(path, src);

    analysis
        .check(path)
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn a_sourcemap_lookup_answers_with_the_child() {
    under_both(|solver| {
        let found = tree_complaints(
            "--!strict\n\
             local markers = game.Workspace:FindFirstChild(\"Markers\")\n\
             local waited = game.Workspace:WaitForChild(\"Markers\")\n\
             local deep = game:FindFirstChild(\"Deep\", true)\n\
             local up = deep:FindFirstAncestor(\"Markers\")\n\
             print(markers.Deep, waited.Deep, deep.CFrame, up.Deep)\n",
        );

        assert!(found.is_empty(), "{solver}: {found:?}");

        let missing = tree_complaints(
            "--!strict\n\
             local none = game.Workspace:FindFirstChild(\"Nope\")\n\
             print(none.Name)\n",
        );

        assert!(
            !missing.is_empty(),
            "{solver}: a name the tree lacks stays optional"
        );
    });
}

#[test]
fn a_tagged_call_completes_its_string() {
    fn offered(src: &str) -> Vec<String> {
        let mut analysis = LuauAnalysis::new();
        let path = Path::new("/strings.luau");

        assert!(analysis.definitions("@sourcemap", TREE));

        analysis.open(path, src);

        analysis
            .completions(path, src.len() as u32)
            .into_iter()
            .map(|c| c.label)
            .collect()
    }

    under_both(|solver| {
        let cases = [
            (
                "local part = Instance.new(\"Part\")\npart:IsA(\"",
                "BasePart",
            ),
            ("game:GetService(\"", "Players"),
            ("Instance.new(\"", "Part"),
            (
                "local part = Instance.new(\"Part\")\npart:GetPropertyChangedSignal(\"",
                "CFrame",
            ),
            ("Enum.KeyCode.A:IsA(\"", "KeyCode"),
            ("game.Workspace:FindFirstChild(\"", "Markers"),
        ];

        for (src, want) in cases {
            let labels = offered(src);

            assert!(
                labels.iter().any(|l| l == want),
                "{solver}: {want} is missing after {src:?}: {labels:?}"
            );
        }
    });
}
