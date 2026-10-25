/*!
`larvae migrate` moves the settings of StyLua, selene, and darklua into
larvae.toml.

larvae already reads a stylua.toml and a selene.toml, so for those two the
command changes no result: it moves the settings into one file. larvae does
not read a darklua file, so its rules start to run under `larvae process`.

Each setting that larvae has a home for moves, and the source file loses it.
A setting that larvae does not know stays where it is, so a key from a later
version of a tool is not lost. A source file with nothing left goes.
*/

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, Item, Table, TableLike, Value};

use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Stylua,
    Selene,
    Darklua,
}

impl Tool {
    const ALL: [Tool; 3] = [Tool::Stylua, Tool::Selene, Tool::Darklua];

    fn name(self) -> &'static str {
        match self {
            Tool::Stylua => "StyLua",
            Tool::Selene => "selene",
            Tool::Darklua => "darklua",
        }
    }

    /// The files each tool reads, in the order it looks for them
    fn files(self) -> &'static [&'static str] {
        match self {
            Tool::Stylua => &crate::commands::init::STYLUA,
            Tool::Selene => &crate::commands::init::SELENE,
            Tool::Darklua => &[".darklua.json", ".darklua.json5"],
        }
    }
}

/// One config file of another tool
struct Source {
    tool: Tool,
    path: PathBuf,
}

/// How a setting joins larvae.toml
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Merge {
    /// Set the key where larvae.toml does not; a table merges key by key
    Absent,
    /// Add the array items that larvae.toml does not hold yet
    Append,
}

/// One setting, and the table of larvae.toml it goes to
struct Move {
    table: Vec<String>,
    key: String,
    value: Item,
    merge: Merge,
}

/// What one source file gives up, and what stays in it
#[derive(Default)]
struct Plan {
    moves: Vec<Move>,
    /// Settings that larvae reads and has no use for, ex: StyLua's `syntax`
    dropped: Vec<String>,
    /// The settings that stay in the source file
    kept: Vec<String>,
    /// Facts the user must know, ex: darklua ran rules that larvae will not
    notes: Vec<String>,
    /// The source as it stays, or None when nothing is left and the file goes
    rest: Option<String>,
}

impl Plan {
    fn to(&mut self, table: &[&str], key: &str, value: Item, merge: Merge) {
        self.moves.push(Move {
            table: table.iter().map(|s| s.to_string()).collect(),
            key: key.to_string(),
            value: clean(value),
            merge,
        });
    }
}

pub fn run(root: &Path, yes: bool) -> Result<ExitCode> {
    let found = detect(root);

    if found.is_empty() {
        eprintln!("No StyLua, selene, or darklua config here, so there is nothing to migrate.");

        return Ok(ExitCode::SUCCESS);
    }

    let checked = match yes {
        true => vec![true; found.len()],

        false => {
            if !std::io::stdin().is_terminal() {
                bail!(
                    "larvae migrate asks which configs to move; run it in a terminal, or pass --yes to move them all"
                );
            }

            match choose(&found, &mut std::io::stdin().lock(), &mut std::io::stderr())? {
                Some(checked) => checked,

                None => {
                    eprintln!("Nothing migrated.");

                    return Ok(ExitCode::SUCCESS);
                }
            }
        }
    };

    let chosen: Vec<&Source> = found
        .iter()
        .zip(&checked)
        .filter(|(_, on)| **on)
        .map(|(s, _)| s)
        .collect();

    if chosen.is_empty() {
        eprintln!("Nothing checked, so nothing migrated.");

        return Ok(ExitCode::SUCCESS);
    }

    let path = root.join("larvae.toml");
    let before = std::fs::read_to_string(&path).ok();

    let mut doc: DocumentMut = before
        .as_deref()
        .unwrap_or("")
        .parse()
        .context("cannot read larvae.toml as TOML")?;

    let mut done = Vec::new();

    for source in chosen {
        let text = std::fs::read_to_string(&source.path)
            .with_context(|| format!("cannot read {}", ui::rel(&source.path)))?;

        let plan = match source.tool {
            Tool::Stylua => plan_stylua(&text),
            Tool::Selene => plan_selene(&text),
            Tool::Darklua => plan_darklua(&text),
        }
        .with_context(|| format!("in {}", ui::rel(&source.path)))?;

        let mut conflicts = Vec::new();

        for m in &plan.moves {
            apply(&mut doc, m, &mut conflicts);
        }

        done.push((source, plan, conflicts));
    }

    /*
    larvae.toml is written and checked before any source file changes. A
    migration that produced a config larvae refuses puts the old file back
    and touches nothing else, so a failed run loses no setting.
    */
    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("cannot write {}", ui::rel(&path)))?;

    if let Err(e) = validate(root, &path) {
        match &before {
            Some(text) => std::fs::write(&path, text)?,

            None => std::fs::remove_file(&path)?,
        }

        return Err(e.context("the migrated larvae.toml does not load, so larvae changed nothing"));
    }

    for (source, plan, conflicts) in &done {
        let name = ui::rel(&source.path);

        match &plan.rest {
            Some(text) => std::fs::write(&source.path, text)
                .with_context(|| format!("cannot write {name}"))?,

            None => std::fs::remove_file(&source.path)
                .with_context(|| format!("cannot delete {name}"))?,
        }

        report(source, plan, conflicts);
    }

    Ok(ExitCode::SUCCESS)
}

fn detect(root: &Path) -> Vec<Source> {
    Tool::ALL
        .iter()
        .filter_map(|&tool| {
            tool.files()
                .iter()
                .map(|name| root.join(name))
                .find(|path| path.is_file())
                .map(|path| Source { tool, path })
        })
        .collect()
}

/*
The checklist. Every config starts checked, a number toggles one, and Enter
accepts the list.

The prompt reads whole lines, not keys. A key-by-key list needs the terminal
in raw mode, which is a different API on every platform and a dependency
larvae would ship to everyone for one prompt. A line works in every terminal
and in every CI log.
*/
fn choose(
    found: &[Source],
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<Option<Vec<bool>>> {
    let color = ui::want_color_stderr();
    let mut checked = vec![true; found.len()];

    writeln!(
        out,
        "{} Migrate these configs into larvae.toml:",
        ui::accent("?", color)
    )?;

    loop {
        for (i, source) in found.iter().enumerate() {
            let mark = match checked[i] {
                true => ui::accent("[x]", color),

                false => "[ ]".to_string(),
            };

            writeln!(
                out,
                "  {} {mark} {:<8} {}",
                i + 1,
                source.tool.name(),
                ui::rel(&source.path)
            )?;
        }

        write!(
            out,
            "  Type a number to check or uncheck it, Enter to migrate, q to quit: "
        )?;
        out.flush()?;

        let mut line = String::new();

        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }

        let line = line.trim();

        if line.is_empty() {
            return Ok(Some(checked));
        }

        if line.eq_ignore_ascii_case("q") {
            return Ok(None);
        }

        for word in line
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|w| !w.is_empty())
        {
            match word.parse::<usize>() {
                Ok(n) if (1..=found.len()).contains(&n) => checked[n - 1] = !checked[n - 1],

                _ => writeln!(out, "  {word} is not a number from 1 to {}", found.len())?,
            }
        }
    }
}

// --- StyLua -------------------------------------------------------------------

/*
Every stylua key that `[fmt]` takes moves there, with stylua's PascalCase
values in larvae's spelling.

A table moves key by key, so `[sort_requires]` gives up the keys larvae
reads and keeps the rest. `syntax` goes without a move: larvae formats Luau
whatever it says.
*/
fn plan_stylua(text: &str) -> Result<Plan> {
    let mut doc: DocumentMut = text.parse().context("cannot read it as TOML")?;
    let known = toml::Value::try_from(crate::fmt::FmtConfig::default())
        .expect("the config always serializes");
    let mut plan = Plan::default();

    for key in keys(doc.as_table()) {
        if key == "syntax" {
            doc.remove(&key);
            plan.dropped.push(key);

            continue;
        }

        let nested = known.get(&key).is_some_and(toml::Value::is_table);

        match doc[&key].as_table_like_mut() {
            Some(inner) if nested => {
                for name in keys_like(inner) {
                    let mut item = inner.get(&name).cloned().unwrap_or_default();
                    respell(&mut item);

                    if fmt_takes(&known, &key, Some(&name), &item) {
                        inner.remove(&name);
                        plan.to(&["fmt", &key], &name, item, Merge::Absent);
                    } else {
                        plan.kept.push(format!("{key}.{name}"));
                    }
                }

                if inner.is_empty() {
                    doc.remove(&key);
                }
            }

            _ => {
                let mut item = doc[&key].clone();
                respell(&mut item);

                if fmt_takes(&known, &key, None, &item) {
                    doc.remove(&key);
                    plan.to(&["fmt"], &key, item, Merge::Absent);
                } else {
                    plan.kept.push(key);
                }
            }
        }
    }

    plan.rest = rest_of(&doc);

    Ok(plan)
}

/// Reports if `[fmt]` takes this key, as larvae's own name and with this value
fn fmt_takes(known: &toml::Value, key: &str, inner: Option<&str>, item: &Item) -> bool {
    let Some(value) = to_toml(item) else {
        return false;
    };

    let (probe, exists) = match inner {
        None => (table_of(key, value), known.get(key).is_some()),

        Some(name) => (
            table_of(key, table_of(name, value)),
            known.get(key).and_then(|t| t.get(name)).is_some(),
        ),
    };

    exists && probe.try_into::<crate::fmt::FmtConfig>().is_ok()
}

/// stylua's enum spelling, ex: `"Tabs"`, in larvae's, ex: `"tabs"`
fn respell(item: &mut Item) {
    fn value(v: &mut Value) {
        match v {
            Value::String(s) => {
                if let Some(kebab) = crate::fmt::config::stylua_spelling(s.value()) {
                    *v = Value::from(kebab);
                }
            }

            Value::Array(items) => items.iter_mut().for_each(value),

            Value::InlineTable(t) => t.iter_mut().for_each(|(_, v)| value(v)),

            _ => {}
        }
    }

    match item {
        Item::Value(v) => value(v),

        Item::Table(t) => t.iter_mut().for_each(|(_, i)| respell(i)),

        _ => {}
    }
}

// --- selene -------------------------------------------------------------------

/*
selene's file has four parts larvae reads: `std`, `exclude`, the levels in
`[lints]`, and the per-lint settings in `[config]`.

A level moves when larvae has a lint of that name. A setting moves when the
larvae lint of that name reads it, so a setting larvae ignores stays in
selene.toml, and a lint larvae lacks keeps its settings there too.
*/
fn plan_selene(text: &str) -> Result<Plan> {
    let mut doc: DocumentMut = text.parse().context("cannot read it as TOML")?;
    let mut plan = Plan::default();

    let lints: Vec<&str> = crate::lint::registry().iter().map(|l| l.name()).collect();

    for key in keys(doc.as_table()) {
        match key.as_str() {
            "std" => {
                let std = doc[&key].as_str().map(str::to_owned);

                match std.filter(|s| crate::lint::config::StdLib::parse(s).is_some()) {
                    Some(_) => {
                        let item = doc.remove(&key).unwrap_or_default();
                        plan.to(&["lint"], "std", item, Merge::Absent);
                    }

                    None => plan.kept.push(key),
                }
            }

            "exclude" => match doc[&key]
                .as_array()
                .is_some_and(|a| a.iter().all(|v| v.is_str()))
            {
                true => {
                    let item = doc.remove(&key).unwrap_or_default();
                    plan.to(&["lint"], "exclude", item, Merge::Append);
                }

                false => plan.kept.push(key),
            },

            "lints" | "rules" => {
                let Some(table) = doc[&key].as_table_like_mut() else {
                    plan.kept.push(key);

                    continue;
                };

                for name in keys_like(table) {
                    let level = table.get(&name).and_then(Item::as_str).map(str::to_owned);

                    let fits = level.as_deref().is_some_and(|l| {
                        toml::Value::String(l.to_owned())
                            .try_into::<crate::lint::config::Level>()
                            .is_ok()
                    });

                    if fits && lints.contains(&name.as_str()) {
                        let item = table.remove(&name).unwrap_or_default();
                        plan.to(&["lint", "rules"], &name, item, Merge::Absent);
                    } else {
                        plan.kept.push(format!("{key}.{name}"));
                    }
                }

                if table.is_empty() {
                    doc.remove(&key);
                }
            }

            "config" => {
                let Some(table) = doc[&key].as_table_like_mut() else {
                    plan.kept.push(key);

                    continue;
                };

                for lint in keys_like(table) {
                    let Some(settings) = table.get_mut(&lint).and_then(Item::as_table_like_mut)
                    else {
                        plan.kept.push(format!("config.{lint}"));

                        continue;
                    };

                    for name in keys_like(settings) {
                        let item = settings.get(&name).cloned().unwrap_or_default();

                        let fits = to_toml(&item).is_some_and(|v| {
                            crate::lint::lints::takes_options(&lint, &table_of(&name, v))
                        });

                        if fits {
                            settings.remove(&name);
                            plan.to(&["lint", "options", &lint], &name, item, Merge::Absent);
                        } else {
                            plan.kept.push(format!("config.{lint}.{name}"));
                        }
                    }

                    if settings.is_empty() {
                        table.remove(&lint);
                    }
                }

                if table.is_empty() {
                    doc.remove(&key);
                }
            }

            _ => plan.kept.push(key),
        }
    }

    plan.rest = rest_of(&doc);

    Ok(plan)
}

// --- darklua ------------------------------------------------------------------

/*
darklua's rules become `[rules]` keys, `inject_global_value` becomes a
`[defines]` entry, and the generator becomes `[process] generator`.

A rule moves when larvae has it and every parameter it carries is one larvae
reads with the same meaning. darklua applies its rules in the order of the
list and larvae in its own order, which no config of larvae can change.

The file is JSON or JSON5. What stays is written back as JSON, which JSON5
reads too, and a JSON5 comment does not survive the rewrite.
*/
fn plan_darklua(text: &str) -> Result<Plan> {
    let mut root: serde_json::Value = json5::from_str(text).context("cannot read it as JSON5")?;

    let Some(object) = root.as_object_mut() else {
        bail!("the file is not a JSON object");
    };

    let mut plan = Plan::default();

    let dense = match object.get("generator") {
        Some(serde_json::Value::String(s)) => s == "dense",

        Some(other) => other.get("name").and_then(|n| n.as_str()) == Some("dense"),

        None => false,
    };

    if let Some(spec) = object.get("generator") {
        match generator(spec) {
            Some((name, span)) => {
                plan.to(
                    &["process"],
                    "generator",
                    Item::Value(name.into()),
                    Merge::Absent,
                );

                if let Some(span) = span {
                    plan.to(
                        &["minify"],
                        "column_span",
                        Item::Value(span.into()),
                        Merge::Absent,
                    );
                }

                object.remove("generator");
            }

            None => plan.kept.push("generator".to_string()),
        }
    }

    match object.get_mut("rules").and_then(|r| r.as_array_mut()) {
        Some(rules) => {
            let mut left = Vec::new();

            for entry in std::mem::take(rules) {
                match darklua_rule(&entry, dense) {
                    Some(RuleMove::Rule(name, value)) => {
                        plan.to(&["rules"], &name, Item::Value(value), Merge::Absent);
                    }

                    Some(RuleMove::Define(name, value)) => {
                        plan.to(&["defines"], &name, Item::Value(value), Merge::Absent);
                    }

                    Some(RuleMove::Redundant(name)) => plan.dropped.push(format!("rules.{name}")),

                    None => {
                        plan.kept.push(format!("rules.{}", rule_name(&entry).unwrap_or("?")));
                        left.push(entry);
                    }
                }
            }

            *rules = left;
        }

        None => plan.notes.push(
            "the file names no rules, so darklua ran its default list; larvae runs no rule until [rules] names one".to_string(),
        ),
    }

    for key in object.keys() {
        if key != "rules" && key != "generator" {
            plan.kept.push(key.clone());
        }
    }

    /*
    An empty `rules` list stays while other keys do. Without the key darklua
    runs its default list, and the rules that list holds now run in larvae.
    */
    let nothing_left = object
        .iter()
        .all(|(k, v)| k == "rules" && v.as_array().is_some_and(Vec::is_empty));

    plan.rest = match nothing_left {
        true => None,

        false => Some(serde_json::to_string_pretty(&root)? + "\n"),
    };

    Ok(plan)
}

enum RuleMove {
    /// A `[rules]` key and its value
    Rule(String, Value),
    /// A `[defines]` name and its literal
    Define(String, Value),
    /// The rule says nothing larvae does not already do
    Redundant(String),
}

fn rule_name(entry: &serde_json::Value) -> Option<&str> {
    match entry {
        serde_json::Value::String(s) => Some(s),

        other => other.get("rule")?.as_str(),
    }
}

fn darklua_rule(entry: &serde_json::Value, dense: bool) -> Option<RuleMove> {
    let name = rule_name(entry)?;

    let params: serde_json::Map<String, serde_json::Value> = match entry {
        serde_json::Value::Object(o) => o
            .iter()
            .filter(|(k, _)| *k != "rule")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),

        _ => serde_json::Map::new(),
    };

    match name {
        /*
        A literal value becomes a define. A value read from an environment
        variable has no `[defines]` form, so that entry stays.
        */
        "inject_global_value" => {
            let ident = params.get("identifier")?.as_str()?;
            let value = params.get("value")?;

            let literal = matches!(
                value,
                serde_json::Value::Bool(_)
                    | serde_json::Value::Number(_)
                    | serde_json::Value::String(_)
            );

            let only_these = params.keys().all(|k| k == "identifier" || k == "value");

            (literal && only_these && crate::rules::native::is_ident(ident))
                .then(|| RuleMove::Define(ident.to_string(), json_to_toml(value).unwrap()))
        }

        // The dense generator already removes every space it can.
        "remove_spaces" if dense && params.is_empty() => {
            Some(RuleMove::Redundant(name.to_string()))
        }

        _ => {
            if !matches!(
                crate::config::rule_status(name),
                Some(crate::config::RuleStatus::Done)
            ) {
                return None;
            }

            if !params
                .keys()
                .all(|k| rule_params(name).contains(&k.as_str()))
            {
                return None;
            }

            let value = match params.is_empty() {
                true => Value::from(true),

                false => json_to_toml(&serde_json::Value::Object(params))?,
            };

            let probe = table_of(name, to_toml(&Item::Value(value.clone()))?);

            probe
                .try_into::<crate::config::RulesConfig>()
                .is_ok()
                .then(|| RuleMove::Rule(name.to_string(), value))
        }
    }
}

/// The darklua parameters that a larvae rule reads under the same name and meaning
fn rule_params(name: &str) -> &'static [&'static str] {
    match name {
        "remove_comments" => &["except"],

        "append_text_comment" => &["text", "file", "location"],

        "remove_attribute" => &["match"],

        "remove_interpolated_string" => &["strategy"],

        "remove_assertions" | "remove_debug_profiling" => &["preserve_arguments_side_effects"],

        _ => &[],
    }
}

/// darklua's generator in larvae's spelling, with the column span of a dense one
fn generator(spec: &serde_json::Value) -> Option<(&'static str, Option<i64>)> {
    let (name, span) = match spec {
        serde_json::Value::String(s) => (s.as_str(), None),

        serde_json::Value::Object(o) => {
            if !o.keys().all(|k| k == "name" || k == "column_span") {
                return None;
            }

            let span = match o.get("column_span") {
                Some(v) => Some(v.as_i64()?),

                None => None,
            };

            (o.get("name")?.as_str()?, span)
        }

        _ => return None,
    };

    let larvae = match name {
        "retain_lines" => "retain-lines",
        "dense" => "dense",
        "readable" => "readable",
        _ => return None,
    };

    // larvae reads the column span for the dense generator only
    if span.is_some() && larvae != "dense" {
        return None;
    }

    Some((larvae, span))
}

fn json_to_toml(value: &serde_json::Value) -> Option<Value> {
    Some(match value {
        serde_json::Value::Null => return None,

        serde_json::Value::Bool(b) => Value::from(*b),

        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::from(i),

            None => Value::from(n.as_f64()?),
        },

        serde_json::Value::String(s) => Value::from(s.as_str()),

        serde_json::Value::Array(items) => {
            let mut array = toml_edit::Array::new();

            for item in items {
                array.push(json_to_toml(item)?);
            }

            Value::Array(array)
        }

        serde_json::Value::Object(map) => {
            let mut table = toml_edit::InlineTable::new();

            for (k, v) in map {
                table.insert(k, json_to_toml(v)?);
            }

            Value::InlineTable(table)
        }
    })
}

// --- larvae.toml --------------------------------------------------------------

/*
Put one setting into larvae.toml.

A key larvae.toml already sets keeps its value: larvae read it over the
other tool's file already, so the result of a run does not change. The
source file still loses its copy, and the report names the key.
*/
fn apply(doc: &mut DocumentMut, m: &Move, conflicts: &mut Vec<String>) {
    let mut path = m.table.clone();

    // selene calls the per-lint table `config`, and a project can too; one spelling only
    if path.len() >= 2
        && path[0] == "lint"
        && path[1] == "options"
        && doc.get("lint").and_then(|l| l.get("config")).is_some()
    {
        path[1] = "config".to_string();
    }

    let at = format!("{}.{}", path.join("."), m.key);

    match table_at(doc.as_table_mut(), &path) {
        Some(table) => put(table, &m.key, m.value.clone(), m.merge, conflicts, &at),

        None => conflicts.push(at),
    }
}

fn table_at<'a>(
    mut current: &'a mut dyn TableLike,
    path: &[String],
) -> Option<&'a mut dyn TableLike> {
    for segment in path {
        if !current.contains_key(segment) {
            let mut table = Table::new();
            table.set_implicit(true);
            current.insert(segment, Item::Table(table));
        }

        current = current.get_mut(segment)?.as_table_like_mut()?;
    }

    Some(current)
}

fn put(
    table: &mut dyn TableLike,
    key: &str,
    value: Item,
    merge: Merge,
    conflicts: &mut Vec<String>,
    at: &str,
) {
    let Some(existing) = table.get_mut(key) else {
        table.insert(key, value);

        return;
    };

    match merge {
        Merge::Append => match (existing.as_array_mut(), value.as_array()) {
            (Some(present), Some(add)) => {
                for item in add.iter() {
                    if !present
                        .iter()
                        .any(|p| p.to_string().trim() == item.to_string().trim())
                    {
                        present.push(item.clone());
                    }
                }
            }

            _ => conflicts.push(at.to_string()),
        },

        Merge::Absent => match (existing.as_table_like_mut(), value.as_table_like()) {
            (Some(present), Some(add)) => {
                for (k, v) in add.iter() {
                    put(
                        present,
                        k,
                        v.clone(),
                        Merge::Absent,
                        conflicts,
                        &format!("{at}.{k}"),
                    );
                }
            }

            _ => conflicts.push(at.to_string()),
        },
    }
}

/// The migrated file must load the way every command loads it
fn validate(root: &Path, path: &Path) -> Result<()> {
    let config = crate::config::Config::load(path)?;

    crate::fmt::FmtConfig::discover(root, config.fmt.as_ref())?;
    crate::lint::LintConfig::discover(root, config.lint.as_ref())?;

    if let Some(defines) = &config.defines {
        crate::rules::defines::parse(defines).map_err(anyhow::Error::msg)?;
    }

    Ok(())
}

fn report(source: &Source, plan: &Plan, conflicts: &[String]) {
    let name = ui::rel(&source.path);
    let moved = plan.moves.len() + plan.dropped.len();

    let settings = |n: usize| match n {
        1 => "1 setting".to_string(),

        n => format!("{n} settings"),
    };

    let fate = match plan.rest {
        None => format!("{name} deleted"),

        Some(_) => format!(
            "{name} keeps {} that larvae does not read: {}",
            settings(plan.kept.len()),
            plan.kept.join(", ")
        ),
    };

    ui::print_success(&format!(
        "{}: {} moved to larvae.toml, {fate}",
        source.tool.name(),
        settings(moved)
    ));

    for key in conflicts {
        eprintln!("  larvae.toml already sets {key}, so its value stays");
    }

    for note in &plan.notes {
        eprintln!("  {note}");
    }
}

// --- helpers ------------------------------------------------------------------

fn keys(table: &Table) -> Vec<String> {
    table.iter().map(|(k, _)| k.to_string()).collect()
}

fn keys_like(table: &dyn TableLike) -> Vec<String> {
    table.iter().map(|(k, _)| k.to_string()).collect()
}

/// The source as it stays, or None when no key is left in it
fn rest_of(doc: &DocumentMut) -> Option<String> {
    // a removed first key leaves the blank line that followed it at the top
    (!doc.as_table().is_empty()).then(|| doc.to_string().trim_start().to_string())
}

/// A value without the comments and spacing it had in the source file
fn clean(mut item: Item) -> Item {
    if let Item::Value(v) = &mut item {
        v.decor_mut().clear();
    }

    item
}

fn table_of(key: &str, value: toml::Value) -> toml::Value {
    toml::Value::Table(toml::Table::from_iter([(key.to_string(), value)]))
}

/// The serde form of one item, for a config struct to test
fn to_toml(item: &Item) -> Option<toml::Value> {
    let mut doc = DocumentMut::new();
    doc.insert("v", item.clone());

    let table: toml::Table = toml::from_str(&doc.to_string()).ok()?;

    table.get("v").cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plans(tool: Tool, text: &str) -> Plan {
        match tool {
            Tool::Stylua => plan_stylua(text),
            Tool::Selene => plan_selene(text),
            Tool::Darklua => plan_darklua(text),
        }
        .unwrap()
    }

    /// The moves as `table.key = value` lines, in order
    fn moved(plan: &Plan) -> Vec<String> {
        plan.moves
            .iter()
            .map(|m| {
                format!(
                    "{}.{} = {}",
                    m.table.join("."),
                    m.key,
                    m.value.to_string().trim()
                )
            })
            .collect()
    }

    #[test]
    fn stylua_keys_move_in_larvae_spelling() {
        let plan = plans(
            Tool::Stylua,
            "column_width = 100\nindent_type = \"Spaces\"\nquote_style = \"AutoPreferSingle\"\nsyntax = \"Luau\"\n",
        );

        assert_eq!(
            moved(&plan),
            [
                "fmt.column_width = 100",
                "fmt.indent_type = \"spaces\"",
                "fmt.quote_style = \"auto-prefer-single\"",
            ]
        );
        assert_eq!(plan.dropped, ["syntax"]);
        assert!(plan.rest.is_none(), "nothing is left, so the file goes");
    }

    /// A key from a later stylua, or a value larvae does not take, stays put.
    #[test]
    fn an_unknown_stylua_key_stays_in_the_file() {
        let plan = plans(
            Tool::Stylua,
            "column_width = 80\nspace_after_semicolons = true\n\n[sort_requires]\nenabled = true\nsomething_new = 1\n",
        );

        assert_eq!(
            moved(&plan),
            ["fmt.column_width = 80", "fmt.sort_requires.enabled = true"]
        );
        assert_eq!(
            plan.kept,
            ["space_after_semicolons", "sort_requires.something_new"]
        );

        let rest = plan.rest.unwrap();

        assert!(rest.contains("space_after_semicolons = true"), "{rest}");
        assert!(rest.contains("something_new = 1"), "{rest}");
        assert!(!rest.contains("column_width"), "{rest}");
        assert!(!rest.contains("enabled"), "{rest}");
    }

    #[test]
    fn selene_moves_what_larvae_lints_read() {
        let plan = plans(
            Tool::Selene,
            concat!(
                "std = \"roblox\"\n",
                "exclude = [\"vendor/*\"]\n\n",
                "[lints]\nshadowing = \"deny\"\nroblox_incorrect_roact_usage = \"allow\"\n\n",
                "[config]\n",
                "unused_variable = { parameters = true, made_up = 1 }\n",
                "empty_if = { comments_count = true }\n",
            ),
        );

        assert_eq!(
            moved(&plan),
            [
                "lint.std = \"roblox\"",
                "lint.exclude = [\"vendor/*\"]",
                "lint.rules.shadowing = \"deny\"",
                "lint.options.unused_variable.parameters = true",
            ]
        );
        assert_eq!(
            plan.kept,
            [
                "lints.roblox_incorrect_roact_usage",
                "config.unused_variable.made_up",
                "config.empty_if.comments_count",
            ]
        );

        let rest = plan.rest.unwrap();

        assert!(rest.contains("roblox_incorrect_roact_usage"), "{rest}");
        assert!(!rest.contains("shadowing"), "{rest}");
        assert!(!rest.contains("std"), "{rest}");
    }

    #[test]
    fn darklua_rules_defines_and_generator_move() {
        let plan = plans(
            Tool::Darklua,
            r#"{
                // a JSON5 comment
                generator: { name: "dense", column_span: 100 },
                rules: [
                    "remove_types",
                    { rule: "remove_comments", except: ["^--!"] },
                    { rule: "inject_global_value", identifier: "DEV", value: false },
                    "remove_spaces",
                ],
            }"#,
        );

        assert_eq!(
            moved(&plan),
            [
                "process.generator = \"dense\"",
                "minify.column_span = 100",
                "rules.remove_types = true",
                "rules.remove_comments = { except = [\"^--!\"] }",
                "defines.DEV = false",
            ]
        );
        assert_eq!(plan.dropped, ["rules.remove_spaces"]);
        assert!(plan.rest.is_none(), "nothing is left, so the file goes");
    }

    /*
    A rule larvae lacks, a parameter it does not read, and a value from the
    environment all stay. The emptied rules list stays beside them, because
    without it darklua would run its default list.
    */
    #[test]
    fn darklua_keeps_what_larvae_cannot_say() {
        let plan = plans(
            Tool::Darklua,
            r#"{
                "bundle": { "require_mode": "path" },
                "rules": [
                    "convert_require",
                    { "rule": "rename_variables", "globals": ["$roblox"] },
                    { "rule": "inject_global_value", "identifier": "ENV", "env": "MY_ENV" }
                ]
            }"#,
        );

        assert!(moved(&plan).is_empty());
        assert_eq!(
            plan.kept,
            [
                "rules.convert_require",
                "rules.rename_variables",
                "rules.inject_global_value",
                "bundle",
            ]
        );

        let all_moved = plans(
            Tool::Darklua,
            r#"{ "bundle": { "require_mode": "path" }, "rules": ["remove_types"] }"#,
        );

        let rest: serde_json::Value = serde_json::from_str(&all_moved.rest.unwrap()).unwrap();

        assert_eq!(rest["rules"], serde_json::json!([]));
        assert_eq!(rest["bundle"]["require_mode"], "path");
    }

    #[test]
    fn darklua_without_rules_says_its_defaults_stop() {
        let plan = plans(Tool::Darklua, r#"{ "generator": "retain_lines" }"#);

        assert_eq!(moved(&plan), ["process.generator = \"retain-lines\""]);
        assert!(plan.notes[0].contains("default list"), "{:?}", plan.notes);
        assert!(plan.rest.is_none());
    }

    /// A key larvae.toml sets keeps its value, and the report names it.
    #[test]
    fn larvae_toml_wins_a_conflict() {
        let mut doc: DocumentMut = "[fmt]\ncolumn_width = 120\n".parse().unwrap();
        let plan = plans(Tool::Stylua, "column_width = 80\nindent_width = 2\n");
        let mut conflicts = Vec::new();

        for m in &plan.moves {
            apply(&mut doc, m, &mut conflicts);
        }

        let text = doc.to_string();

        assert!(text.contains("column_width = 120"), "{text}");
        assert!(text.contains("indent_width = 2"), "{text}");
        assert_eq!(conflicts, ["fmt.column_width"]);
    }

    /// selene's `config` spelling in larvae.toml takes the settings, so no second table appears.
    #[test]
    fn lint_options_join_an_existing_config_table() {
        let mut doc: DocumentMut = "[lint.config.deprecated]\nambiguous_methods = true\n"
            .parse()
            .unwrap();
        let plan = plans(
            Tool::Selene,
            "[config]\nunused_variable = { parameters = true }\n",
        );

        for m in &plan.moves {
            apply(&mut doc, m, &mut Vec::new());
        }

        let text = doc.to_string();

        assert!(text.contains("[lint.config.unused_variable]"), "{text}");
        assert!(!text.contains("options"), "{text}");
    }

    #[test]
    fn exclude_lists_merge() {
        let mut doc: DocumentMut = "[lint]\nexclude = [\"a/*\"]\n".parse().unwrap();
        let plan = plans(Tool::Selene, "exclude = [\"a/*\", \"b/*\"]\n");

        for m in &plan.moves {
            apply(&mut doc, m, &mut Vec::new());
        }

        assert!(
            doc.to_string().contains("exclude = [\"a/*\", \"b/*\"]"),
            "{doc}"
        );
    }

    fn found() -> Vec<Source> {
        Tool::ALL
            .iter()
            .map(|&tool| Source {
                tool,
                path: PathBuf::from(tool.files()[0]),
            })
            .collect()
    }

    #[test]
    fn every_config_starts_checked_and_a_number_toggles_it() {
        let mut out = Vec::new();

        let chosen = choose(&found(), &mut "2\n\n".as_bytes(), &mut out).unwrap();

        assert_eq!(chosen, Some(vec![true, false, true]));

        let shown = String::from_utf8(out).unwrap();

        assert!(shown.contains("[ ]"), "{shown}");
    }

    #[test]
    fn q_or_the_end_of_input_migrates_nothing() {
        assert_eq!(
            choose(&found(), &mut "q\n".as_bytes(), &mut Vec::new()).unwrap(),
            None
        );
        assert_eq!(
            choose(&found(), &mut "".as_bytes(), &mut Vec::new()).unwrap(),
            None
        );
    }

    #[test]
    fn a_word_that_is_no_number_is_named() {
        let mut out = Vec::new();

        let chosen = choose(&found(), &mut "7 x\n\n".as_bytes(), &mut out).unwrap();

        assert_eq!(chosen, Some(vec![true, true, true]));
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("7 is not a number from 1 to 3")
        );
    }

    /// The whole run: larvae.toml loads, the emptied files go, and the rest stays.
    #[test]
    fn a_run_moves_every_config_and_larvae_toml_loads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(root.join("larvae.toml"), "# my project\ninput = \"src\"\n").unwrap();
        std::fs::write(
            root.join("stylua.toml"),
            "indent_type = \"Spaces\"\nindent_width = 2\n",
        )
        .unwrap();
        std::fs::write(
            root.join("selene.toml"),
            "std = \"roblox\"\n\n[lints]\nshadowing = \"deny\"\nmade_up_lint = \"warn\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".darklua.json"),
            r#"{ "rules": ["remove_types", { "rule": "inject_global_value", "identifier": "DEV", "value": true }] }"#,
        )
        .unwrap();

        run(root, true).unwrap();

        let larvae = std::fs::read_to_string(root.join("larvae.toml")).unwrap();

        assert!(larvae.starts_with("# my project\n"), "{larvae}");
        assert!(larvae.contains("indent_type = \"spaces\""), "{larvae}");
        assert!(larvae.contains("shadowing = \"deny\""), "{larvae}");
        assert!(larvae.contains("remove_types = true"), "{larvae}");
        assert!(larvae.contains("DEV = true"), "{larvae}");

        assert!(!root.join("stylua.toml").exists());
        assert!(!root.join(".darklua.json").exists());

        let selene = std::fs::read_to_string(root.join("selene.toml")).unwrap();

        assert!(selene.contains("made_up_lint"), "{selene}");
        assert!(!selene.contains("shadowing"), "{selene}");

        let config = crate::config::Config::load(&root.join("larvae.toml")).unwrap();

        assert!(config.rules.remove_types);
    }

    #[test]
    fn a_config_that_will_not_load_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // a broken larvae.toml value elsewhere makes the migrated file fail to load
        std::fs::write(
            root.join("larvae.toml"),
            "generator_that_does_not_exist = 1\n",
        )
        .unwrap();
        std::fs::write(root.join("stylua.toml"), "column_width = 90\n").unwrap();

        assert!(run(root, true).is_err());

        assert_eq!(
            std::fs::read_to_string(root.join("larvae.toml")).unwrap(),
            "generator_that_does_not_exist = 1\n"
        );
        assert!(root.join("stylua.toml").exists());
    }
}
