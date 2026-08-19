/*!
Turning what a project wrote into what a worm receives.

A worm declares its rules and its settings in `worm.toml`, and a project says
what it wants under `[worms.<name>]`. These functions meet the two: a name the
worm does not declare is an error rather than a value quietly ignored, and a
default the worm gave stands where the project said nothing.
*/

use std::collections::BTreeMap;

use anyhow::{Result, bail};

use super::Worm;
use crate::worm::manifest::RuleDecl;

/*
The value of the user wins over the manifest default. An off rule is absent,
so a worm does not see a rule it must not run. A rule the worm did not declare
is an error. Without the error, a typo in [rules] is a setting that does
nothing.
*/
pub(super) fn resolve_rules(
    name: &str,
    worm: &Worm,
    rules: &BTreeMap<String, toml::Value>,
) -> Result<BTreeMap<String, toml::Value>> {
    let mut out = BTreeMap::new();

    /*
    A rule the user switched on, which this worm does not declare, would be a
    setting that silently does nothing. Thus larvae names it and does not
    ignore it.
    */
    for key in rules.keys() {
        if !worm.manifest.rules.contains_key(key) {
            bail!("worm `{name}` has no rule `{key}`");
        }
    }

    for (rule, decl) in &worm.manifest.rules {
        let user = rules.get(rule);
        let resolved = decl.resolve(user);

        if RuleDecl::is_off(resolved) {
            continue;
        }

        out.insert(
            rule.clone(),
            resolved.cloned().unwrap_or(toml::Value::Boolean(true)),
        );
    }

    Ok(out)
}

/*
Check the settings of a project against the options the worm declares, and
fill each missing key with its default.

A worm that declares no option keeps the opaque table it always had. A worm
that declares its options gets a complete table at init, so the guest reads a
key instead of a key and a fallback.
*/
pub(super) fn resolve_config(name: &str, worm: &Worm, user: &toml::Value) -> Result<toml::Value> {
    let declared = &worm.manifest.options;

    if declared.is_empty() {
        return Ok(user.clone());
    }

    let mut out = user.as_table().cloned().unwrap_or_default();

    /*
    A key the worm does not declare is a setting that does nothing. It is
    named here, for the same reason a rule the worm does not declare is.
    */
    for (key, value) in &out {
        let Some(option) = declared.get(key) else {
            bail!("worm `{name}` has no option `{key}`");
        };

        if !option.kind.accepts(value) {
            bail!(
                "worm `{name}`: option `{key}` takes a {}",
                option.kind.name()
            );
        }

        if !option.values.is_empty() && !option.values.contains(value) {
            let allowed: Vec<String> = option.values.iter().map(scalar).collect();

            bail!(
                "worm `{name}`: option `{key}` takes one of {}",
                allowed.join(", ")
            );
        }
    }

    for (key, option) in declared {
        if let Some(default) = &option.default
            && !out.contains_key(key)
        {
            out.insert(key.clone(), default.clone());
        }
    }

    Ok(toml::Value::Table(out))
}

/// One value as a user would write it, for a message that lists the choices
pub(super) fn scalar(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => format!("{s:?}"),

        toml::Value::Integer(n) => n.to_string(),

        toml::Value::Float(f) => f.to_string(),

        toml::Value::Boolean(b) => b.to_string(),

        other => other.type_str().to_owned(),
    }
}
