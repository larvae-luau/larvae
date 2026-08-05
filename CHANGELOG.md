# Changelog

Notable changes land here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), versions follow
[semver](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- Require rewriting with three output targets, native Roblox string requires,
  filesystem paths for Lune, and Instance expressions with `find_first_child`,
  `wait_for_child` or `property` indexing
- Aliases from `larvae.toml` and `.luaurc`, merged per key, with chain and
  cycle handling
- Realm and container validation, client code cannot require server only
  containers and Starter containers only ever get relative requires
- Rojo integration, mounts derived from `default.project.json` and a build
  project written to `.larvae/build.project.json`
- A Luau parser and printer, round trips byte for byte, used by `check` to
  report syntax errors
- Incremental builds keyed on a resolution epoch, plus `process --watch`
- Rules, `const_requires`, `remove_comments`, `append_text_comment` and
  `add_luau_directive`, with every darklua rule name accepted
- `larvae init`, `larvae self code`, and `larvae self install`, `update`
  and `uninstall`
